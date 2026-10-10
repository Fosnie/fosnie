from copy import deepcopy
import json
from pathlib import Path
import tempfile
import unittest

import httpx
from cloud_allocation import BoundedCloudProxy, MODELS, PATHS, REASONING_CASE_IDS, expected_body, make_cloud_plan, make_reasoning_plan
from judge_review import digest
from test_cloud_allocation import Handler


class ReasoningAllocationTest(unittest.TestCase):
    def setUp(self):
        self.dataset = {'schema_version': 1, 'dataset_id': 'reasoning-synthetic-fixture',
                        'source': {'kind': 'synthetic', 'reference': 'contract only'},
                        'cases': [{'id': i, 'family_id': '/'.join(i.split('/')[:2]), 'split': 'test', 'language': 'en', 'conversation': 'Question '+i}
                                  for i in sorted(REASONING_CASE_IDS)]}
        self.bank = {'dataset_sha256': digest(self.dataset)};self.manifest = {'version': 'synthetic'}
        self.plan = make_reasoning_plan(self.dataset, self.bank, self.manifest)

    def test_factorial_plan_has_same_questions_and_output_cap_for_both_efforts(self):
        self.assertEqual((self.plan['maximum_calls'], self.plan['maximum_calls_per_model'], self.plan['maximum_output_tokens']), (16, 8, 2048))
        for name in MODELS:
            for variant in ('default', 'high'):
                slots = [r for r in self.plan['requests'] if r['name'] == name and r['variant'] == variant]
                self.assertEqual({r['case_id'] for r in slots}, REASONING_CASE_IDS)
                self.assertTrue(all(r['effort'] == (None if variant == 'default' else 'high') for r in slots))
        self.assertFalse(self.plan['fresh_holdout']);self.assertEqual(self.plan['approval_status'], 'pending')
        original = make_cloud_plan(self.dataset, self.bank, self.manifest)
        self.assertEqual(original['maximum_output_tokens'], 512);self.assertNotIn('variant', original['requests'][0])

    def test_model_case_variant_slots_are_single_use_and_strictly_pinned(self):
        calls = [];now = [0]
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp)/'ledger.jsonl'
            def response(request):
                calls.append(json.loads(request.content))
                self.assertEqual(sum(json.loads(line)['event'] == 'dispatch' for line in path.read_text().splitlines()), len(calls))
                return httpx.Response(200, json={'usage': {'input_tokens': 1, 'output_tokens': 65, 'total_tokens': 66,
                                                         'output_tokens_details': {'reasoning_tokens': 64}}})
            proxy = BoundedCloudProxy({}, self.plan, self.dataset, self.bank, self.manifest, path,
                                      simulation=True, transport=httpx.MockTransport(response), clock=lambda: now[0])
            try:
                for slot in self.plan['requests']:
                    name = slot['name'];case = next(c for c in self.dataset['cases'] if c['id'] == slot['case_id']);now[0] += 10
                    body = expected_body(name, case, output_tokens=2048, effort=slot['effort'])
                    proxy.expect(name, case['id'], slot['variant'])
                    changed = deepcopy(body)
                    if name == 'openai':changed.pop('reasoning', None) if slot['effort'] else changed.update(reasoning={'effort': 'high'})
                    else:changed['generationConfig']['maxOutputTokens'] = 512
                    bad = Handler(changed, PATHS[name]);proxy.forward(bad);self.assertEqual(bad.status, 403)
                    good = Handler(body, PATHS[name]);proxy.forward(good);self.assertEqual(good.status, 200)
                    with self.assertRaises(ValueError):proxy.expect(name, case['id'], slot['variant'])
                self.assertEqual(len(calls), 16);self.assertEqual(len(proxy.records), 16)
                self.assertTrue(all(r['output_cap'] == 2048 and r['source'] == 'synthetic' for r in proxy.records))
                self.assertTrue(all(r['usage']['reasoning_tokens'] == 64 for r in proxy.records))
                with self.assertRaises(ValueError):proxy.expect('openai', self.dataset['cases'][0]['id'])
            finally:proxy.close()


if __name__ == '__main__':unittest.main()
