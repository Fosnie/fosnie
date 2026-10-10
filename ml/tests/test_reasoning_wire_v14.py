# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Reasoning wire fidelity and privacy; all provider calls use MockTransport."""
import json
from uuid import uuid4

import httpx
import pytest
from app import gemini_adapter as ga, http_client, llm, rag_ctx
from app.inference_target import records


@pytest.fixture
def anyio_backend():return 'asyncio'


@pytest.fixture(autouse=True)
def context():
    rag_ctx.set_overrides({});llm.set_stage(None)
    yield
    rag_ctx.set_overrides({});llm.set_stage(None)


def routed_overrides(effort, trace=False):
    assignment = {'schema_version': 1, 'provider_id': str(uuid4()), 'provider_revision': 1, 'profile_revision': 1,
                  'adapter': 'openai_responses', 'base_url': 'https://wire.test/v1', 'model': 'gpt-5.4-mini',
                  'api_key': 'synthetic-key', 'effort': effort}
    return {'routing': {'schema_version': 1, 'decision_id': str(uuid4()), 'purpose': 'primary', 'assignments': {'primary': assignment}},
            'llm_reasoning_trace': str(trace).lower()}


@pytest.mark.anyio
@pytest.mark.parametrize('effort,trace', [('xhigh', False), ('high', True), (None, False)])
async def test_routed_responses_stream_preserves_effort_and_requests_only_enabled_summary(monkeypatch, effort, trace):
    seen = []
    def response(request):
        seen.append(json.loads(request.content))
        data = [{'type': 'response.reasoning_summary_text.delta', 'delta': 'synthetic summary'},
                {'type': 'response.output_text.delta', 'delta': 'ok'},
                {'type': 'response.completed', 'response': {'usage': {'input_tokens': 3, 'output_tokens': 2, 'total_tokens': 5}}}]
        return httpx.Response(200, text=''.join('data: '+json.dumps(row)+'\n\n' for row in data), headers={'content-type': 'text/event-stream'})
    rag_ctx.set_overrides(routed_overrides(effort, trace))
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
        monkeypatch.setattr(http_client, 'get_client', lambda: client)
        events = [e async for e in llm.stream_chat([{'role': 'user', 'content': 'question'}], {'max_tokens': 2048})]
    assert len(seen) == 1 and seen[0]['max_output_tokens'] == 2048
    if effort is None:assert 'reasoning' not in seen[0]
    else:
        assert seen[0]['reasoning']['effort'] == effort
        assert ('summary' in seen[0]['reasoning']) is trace
    assert any(e['type'] == 'reasoning' for e in events) is trace
    assert ''.join(e['delta'] for e in events if e['type'] == 'token') == 'ok'
    assert records()[0]['status'] == 'completed'


@pytest.mark.anyio
async def test_legacy_responses_xhigh_does_not_become_high(monkeypatch):
    seen = []
    def response(request):
        seen.append(json.loads(request.content))
        return httpx.Response(200, text='data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}\n\n')
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
        monkeypatch.setattr(http_client, 'get_client', lambda: client)
        _ = [e async for e in llm._stream_responses('https://wire.test/v1', 'gpt-5.4-mini', [], {'max_tokens': 8}, 'xhigh', False, None)]
    assert seen[0]['reasoning'] == {'effort': 'xhigh'}
    monkeypatch.setattr(http_client, 'get_client', lambda: (_ for _ in ()).throw(AssertionError('invalid effort cannot lookup HTTP')))
    with pytest.raises(ValueError, match='unsupported Responses'):
        _ = [e async for e in llm._stream_responses('https://wire.test/v1', 'gpt-5.4-mini', [], {}, 'invalid', True, None)]


@pytest.mark.anyio
@pytest.mark.parametrize('method', ['complete', 'chat_step'])
async def test_routed_responses_nonstreaming_keeps_explicit_high(monkeypatch, method):
    seen = []
    def response(request):
        seen.append(json.loads(request.content))
        return httpx.Response(200, json={'status': 'completed', 'output': [{'type': 'message', 'content': [{'type': 'output_text', 'text': 'ok'}]}],
            'usage': {'input_tokens': 2, 'output_tokens': 3, 'total_tokens': 5}})
    rag_ctx.set_overrides(routed_overrides('high'))
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
        monkeypatch.setattr(http_client, 'get_client', lambda: client)
        if method == 'complete':result = await llm.complete('system', 'user', max_tokens=2048)
        else:result = await llm.chat_step([{'role': 'user', 'content': 'user'}], sampling={'max_tokens': 2048})
    assert (result if method == 'complete' else result['content']) == 'ok'
    assert len(seen) == 1 and seen[0]['reasoning'] == {'effort': 'high'} and seen[0]['max_output_tokens'] == 2048


@pytest.mark.anyio
@pytest.mark.parametrize('configured,actual,expected', [
    ('gemini-2.5-flash', 'gemini-3.1-flash-lite', {'thinkingLevel': 'HIGH', 'includeThoughts': False}),
    ('gemini-3.1-flash-lite', 'gemini-2.5-flash', {'thinkingBudget': 16384, 'includeThoughts': False})])
async def test_legacy_gemini_translation_uses_actual_model_override(monkeypatch, configured, actual, expected):
    seen = []
    def response(request):
        seen.append((str(request.url), json.loads(request.content)))
        return httpx.Response(200, json={'candidates': [{'content': {'parts': [{'text': 'ok'}]}, 'finishReason': 'STOP'}],
            'usageMetadata': {'promptTokenCount': 2, 'candidatesTokenCount': 1, 'totalTokenCount': 3}})
    rag_ctx.set_overrides({'llm_base_url': 'https://generativelanguage.googleapis.com/v1beta', 'llm_model': configured,
                          'llm_reasoning_enabled': 'true', 'llm_reasoning_level': 'high', 'llm_reasoning_trace': 'false'})
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
        monkeypatch.setattr(http_client, 'get_client', lambda: client)
        result = await llm.chat_step([{'role': 'user', 'content': 'user'}], model=actual, sampling={'max_tokens': 2048})
    assert result['content'] == 'ok' and len(seen) == 1 and actual in seen[0][0]
    gen = seen[0][1]['generationConfig'];assert gen['thinkingConfig'] == expected
    assert gen['maxOutputTokens'] == 2048
    assert not ('thinkingBudget' in gen['thinkingConfig'] and 'thinkingLevel' in gen['thinkingConfig'])


def test_gemini3_default_auto_minimal_and_disabled_are_distinct():
    rag_ctx.set_overrides({'llm_model': 'gemini-3.1-flash-lite'})
    assert ga._thinking_config() is None
    rag_ctx.set_overrides({'llm_model': 'gemini-3.1-flash-lite', 'llm_reasoning_enabled': 'true', 'llm_reasoning_level': 'auto'})
    assert ga._thinking_config() == {'includeThoughts': True}
    rag_ctx.set_overrides({'llm_model': 'gemini-3.1-flash-lite', 'llm_reasoning_enabled': 'true', 'llm_reasoning_level': 'minimal'})
    assert ga._thinking_config()['thinkingLevel'] == 'MINIMAL'
    rag_ctx.set_overrides({'llm_model': 'gemini-3.1-flash-lite', 'llm_reasoning_enabled': 'false'})
    with pytest.raises(ValueError, match='cannot guarantee thinking off'):ga._thinking_config()
    rag_ctx.set_overrides({'llm_model': 'gemini-3.1-flash-lite', 'llm_thinking': 'adaptive:high'})
    assert ga._thinking_config()['thinkingLevel'] == 'HIGH'


@pytest.mark.anyio
@pytest.mark.parametrize('model,effort', [('gemini-3-pro-preview', 'medium'), ('models/gemini-3.1-pro-preview', 'minimal')])
async def test_known_unsupported_gemini_level_fails_before_http(monkeypatch, model, effort):
    monkeypatch.setattr(http_client, 'get_client', lambda: (_ for _ in ()).throw(AssertionError('unsupported model/level cannot dispatch')))
    rag_ctx.set_overrides({'llm_base_url': 'https://generativelanguage.googleapis.com/v1beta', 'llm_model': model,
                          'llm_reasoning_enabled': 'true', 'llm_reasoning_level': effort})
    with pytest.raises(ValueError, match='parameter mapping'):
        await llm.chat_step([{'role': 'user', 'content': 'user'}])


def test_explicit_legacy_sampling_effort_does_not_enable_disabled_trace():
    rag_ctx.set_overrides({'llm_reasoning_trace': 'false'})
    assert llm._reasoning_request({'reasoning_effort': 'xhigh'}) == {'enabled': True, 'level': 'xhigh', 'trace': False}
