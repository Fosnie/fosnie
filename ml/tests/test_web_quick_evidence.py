# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json

import httpx
import pytest
from app import http_client, rag_ctx
from app.inference_budget import BudgetExhausted
from app.web import loop, pipeline, quick_evidence as quick
from test_inference_fixed import fixed
from test_inference_target import clean_context, anyio_backend


def test_late_fact_survives_long_page_and_retains_verbatim_caveat():
    fact = "The unit parameter is scale=imperial. Do not apply it to historical archives."
    text = "General API documentation. " * 100 + fact + " Further details." * 60
    pairs = [(0, "Generic introductory material " * 80) for _ in range(15)] + [(0, text)]
    selected, partial = quick.shortlist("API documentation imperial historical archives", pairs)
    assert partial and len(selected) <= 8
    assert all(len(excerpt) <= 800 for _, excerpt in selected)
    assert any(fact in excerpt for _, excerpt in selected)
    assert all(excerpt in pairs[-1][1] or excerpt in pairs[0][1] for _, excerpt in selected)
    assert pairs[-1][1] == text  # source pool untouched


def test_shortlist_and_digest_preserve_independent_sources_under_keyword_flood():
    pairs = [(0, "target " * 200) for _ in range(30)] + [(1, "Independent target caveat"), (2, "No target result")]
    selected, partial = quick.shortlist("target caveat", pairs)
    assert partial and {source for source, _ in selected} == {0, 1, 2}
    scores = [100.0 if source == 0 else 0.1 for source, _ in selected]
    picked = quick.pick(selected, scores)
    assert len(picked) == 4 and {source for source, _ in picked} == {0, 1, 2}


def test_unicode_focus_and_zero_overlap_are_stable_without_inventing_evidence():
    fact = "По умолчанию единицы — метрические, исключение: архив."
    pairs = [(7, "справка " * 200 + fact + " конец" * 180), (9, "説明文" * 400)]
    first = quick.shortlist("метрические архив", pairs)
    assert first == quick.shortlist("метрические архив", pairs)
    assert any(fact in excerpt for _, excerpt in first[0])
    assert all(excerpt in pairs[0 if source == 7 else 1][1] for source, excerpt in first[0])
    assert quick.shortlist("unrelated", [(2, "small"), (4, "other")]) == ([(2, "small"), (4, "other")], False)


@pytest.mark.parametrize("scores", [[1.0], [1.0, float('nan')], [float('inf'), 0.0]])
def test_malformed_rank_is_not_successful_evidence(scores):
    with pytest.raises(ValueError, match="complete and finite"):
        quick.pick([(0, "one"), (1, "two")], scores)


def _collected():
    pool = loop._Pool()
    for source in range(2):
        pool.upgrade_fetched(pipeline._Source(
            url=f"https://public.example/{source}", title=f"Document {source}", domain="public.example",
            published_date=None, fetched_at="2026-10-05T12:00:00+00:00", snippet_only=False,
            chunks=[f"Source {source} section {i}. " + "General reference text. " * 80 +
                    ("The exact option is scale=imperial, except in historical archives." if i == 9 else "")
                    for i in range(10)]))
    return loop.CollectResult(pool, [(loop._SubQ("broad reference", ["broad reference"], "any"), [0, 1])], [], False)


@pytest.mark.anyio
@pytest.mark.parametrize("streaming", [False, True])
@pytest.mark.parametrize("depth", ["quick", "standard"])
async def test_endpoint_retains_fixed_ledger_and_source_lineage_without_changing_engine_query(monkeypatch, streaming, depth):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings, "shared_secret", "quick-test-secret")
    collected = _collected()
    original = [list(source.chunks) for source in collected.pool.sources]
    queries, calls = [], []

    async def collect(query, recency, budget):
        queries.append((query, recency))
        return collected

    def respond(request):
        payload = json.loads(request.content)
        calls.append((request, payload))
        return httpx.Response(200, json={"results": [{"index": i, "relevance_score": 1.0 if "scale=imperial" in doc else 0.0}
                                                     for i, doc in enumerate(payload['documents'])]})

    monkeypatch.setattr(loop, "collect", collect)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url="http://ml.test") as api:
            response = await api.post('/web_search', headers={"X-Fosnie-ML-Key": "quick-test-secret"}, json={
                "query": "broad reference", "ranking_question": "Find the exact imperial option and archive exception",
                "depth": depth, "stream": streaming, "overrides": {"routing": fixed('rerank', input_bytes=100000, timeout_ms=2000)}})
    assert response.status_code == 200, response.text
    result = json.loads(response.text.splitlines()[-1]) if streaming else response.json()
    assert queries == [("broad reference", "any")]
    assert len(calls) == 1 and calls[0][0].url.host == 'fixed.invalid'
    assert calls[0][1]['model'] == 'fixed-model'
    assert 'authorization' not in calls[0][0].headers
    docs = calls[0][1]['documents']
    if depth == 'quick':
        assert len(docs) <= 8 and all(len(doc) <= 800 for doc in docs)
        assert 'imperial' in calls[0][1]['query']
        assert len(result['citations']) <= 4 and 'coverage is partial' in result['digest']
    else:
        assert len(docs) == 20 and max(map(len, docs)) > 800
        assert calls[0][1]['query'] == 'broad reference'
        assert len(result['citations']) == 10 and 'coverage is partial' not in result['digest']
    assert 'scale=imperial' in result['digest']
    assert {c['url'] for c in result['citations']} <= {'https://public.example/0', 'https://public.example/1'}
    assert all(not c['snippet_only'] and c['fetched_at'] == '2026-10-05T12:00:00+00:00' for c in result['citations'])
    assert [source.chunks for source in collected.pool.sources] == original
    report = result['fixed_routing_report']
    assert report['complete'] and len(report['calls']) == 1
    assert report['calls'][0]['status'] == 'completed'
    assert report['calls'][0]['input_bytes'] == len(calls[0][0].content)
    assert report['calls'][0]['role'] == 'rerank'


@pytest.mark.anyio
async def test_quick_budget_failure_never_dispatches_or_salvages(monkeypatch):
    monkeypatch.setattr(loop, 'collect', lambda *a: _collect_async())
    monkeypatch.setattr(http_client, 'get_client', lambda: pytest.fail('forbidden default target'))
    # The fixed pre-dispatch byte check must fail before the client sends.
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda request: pytest.fail('budget bypass'))) as client:
        monkeypatch.setattr(http_client, 'get_client', lambda: client)
        rag_ctx.set_overrides({'routing': fixed('rerank', input_bytes=1)})
        with pytest.raises(BudgetExhausted):
            await loop.run('reference', depth='quick')


async def _collect_async():
    return _collected()


@pytest.mark.anyio
async def test_endpoint_rejects_oversized_focus_before_search(monkeypatch):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings, 'shared_secret', 'quick-test-secret')
    monkeypatch.setattr(loop, 'collect', lambda *a: pytest.fail('invalid request reached search'))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url='http://ml.test') as api:
        response = await api.post('/web_search', headers={'X-Fosnie-ML-Key': 'quick-test-secret'},
                                  json={'query': 'public docs', 'ranking_question': 'Я' * 2049})
    assert response.status_code == 422
