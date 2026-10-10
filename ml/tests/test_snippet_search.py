# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import unittest
from unittest.mock import AsyncMock, patch

from app import rag_ctx
from app.web import loop
from app.web.provider import SerpResult


class SnippetOnlyTests(unittest.IsolatedAsyncioTestCase):
    async def test_explicit_quick_preserves_source_and_uses_no_inference_or_fetch(self):
        rag_ctx.set_overrides({'web_quick_snippets_only': True, 'web_domain_allowlist': 'weather.test',
                              'web_allowlist_only': True, 'web_ranking_question': 'Glasgow current weather'})
        results = [SerpResult('https://weather.test/glasgow', 'Weather', 'Forecast 12 C tomorrow, not a current observation.', '2026-10-10', 'fixture'),
                   SerpResult('https://blocked.test/report', 'Blocked', 'private canary', None, 'fixture')]
        with patch('app.web.pipeline._serp', AsyncMock(return_value=results)) as serp, \
             patch('app.web.loop.collect', AsyncMock(side_effect=AssertionError('no full collection'))), \
             patch('app.web.loop.reranker.rerank', AsyncMock(side_effect=AssertionError('no inference'))), \
             patch('app.web.pipeline._fetch_source', AsyncMock(side_effect=AssertionError('no fetch'))):
            result = await loop.run('Glasgow weather', 'day', 'quick')
        serp.assert_awaited_once_with('Glasgow weather', 'day', 5)
        self.assertIn('not a current observation', result['digest'])
        self.assertNotIn('private canary', str(result))
        self.assertEqual(len(result['citations']), 1)
        self.assertIn('Snippet-only', result['digest'])
        rag_ctx.set_overrides({})

    async def test_empty_results_are_honest_and_standard_does_not_use_shortcut(self):
        rag_ctx.set_overrides({'web_quick_snippets_only': True})
        with patch('app.web.pipeline._serp', AsyncMock(return_value=[])):
            result = await loop.run('weather', 'day', 'quick')
        self.assertEqual(result['citations'], [])
        self.assertIn('Do not guess', result['digest'])
        with patch('app.web.snippet_search.run', AsyncMock(side_effect=AssertionError('not quick'))) as quick, \
             patch('app.web.loop.collect', AsyncMock(side_effect=RuntimeError('ordinary pipeline'))) as collect:
            with self.assertRaisesRegex(RuntimeError, 'ordinary pipeline'):
                await loop.run('weather', 'day', 'standard')
        quick.assert_not_awaited()
        collect.assert_awaited_once()
        rag_ctx.set_overrides({})
