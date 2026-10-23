from unittest.mock import patch
from app.gemini_adapter import _map_usage

def test_conserved_proto3_usage_has_known_zero_cache_and_thoughts():
    with patch('app.gemini_adapter.strict',return_value=True):
        usage=_map_usage({'promptTokenCount':55,'candidatesTokenCount':3,'totalTokenCount':58})
    assert usage['prompt_tokens']==55 and usage['completion_tokens']==3
    assert usage['reasoning_tokens']==0 and usage['prompt_tokens_details']['cached_tokens']==0
    assert 'cachedContentTokenCount' not in usage['raw_provider_usage']

def test_reasoning_is_in_output_once_and_explicit_cache_is_retained():
    with patch('app.gemini_adapter.strict',return_value=True):
        usage=_map_usage({'promptTokenCount':100,'candidatesTokenCount':4,'thoughtsTokenCount':9,'totalTokenCount':113,'cachedContentTokenCount':20})
    assert usage['completion_tokens']==13 and usage['reasoning_tokens']==9
    assert usage['prompt_tokens_details']['cached_tokens']==20

def test_missing_or_nonconserved_usage_does_not_invent_zero_cache():
    with patch('app.gemini_adapter.strict',return_value=True):
        missing=_map_usage(None)
        partial=_map_usage({'promptTokenCount':55,'candidatesTokenCount':3})
        contradictory=_map_usage({'promptTokenCount':55,'candidatesTokenCount':3,'totalTokenCount':59})
    assert missing['prompt_tokens'] is None and missing['completion_tokens'] is None
    assert partial['prompt_tokens_details']['cached_tokens'] is None
    assert contradictory['prompt_tokens_details']['cached_tokens'] is None

def test_malformed_native_counters_remain_unknown_without_arithmetic_failure():
    for field,value in [('thoughtsTokenCount',None),('candidatesTokenCount','3'),('promptTokenCount',True),('totalTokenCount',-1)]:
        raw={'promptTokenCount':55,'candidatesTokenCount':3,'totalTokenCount':58,field:value}
        with patch('app.gemini_adapter.strict',return_value=True):usage=_map_usage(raw)
        assert usage['prompt_tokens_details']['cached_tokens'] is None
    with patch('app.gemini_adapter.strict',return_value=True):usage=_map_usage({'candidatesTokenCount':3})
    assert usage['prompt_tokens'] is None and usage['total_tokens'] is None
