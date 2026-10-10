from copy import deepcopy
import io
import json
from pathlib import Path
import tempfile
import unittest

import httpx
from cloud_allocation import BoundedCloudProxy, MODELS, PATHS, URLS, expected_body, make_cloud_plan
from judge_review import digest


class Handler:
    def __init__(self, body, path):
        data = json.dumps(body).encode();self.headers = {'Content-Length': str(len(data))}
        self.rfile = io.BytesIO(data);self.wfile = io.BytesIO();self.path = path;self.status = None
    def send_response(self, value):self.status = value
    def send_header(self, *args):pass
    def end_headers(self):pass


class CloudAllocationTest(unittest.TestCase):
    def setUp(self):
        self.dataset = {'schema_version': 1, 'dataset_id': 'synthetic-native-wire-test',
                        'source': {'kind': 'synthetic', 'reference': 'contract only, no real quality'}, 'cases': [
                            {'id': 'cal', 'family_id': 'cal', 'split': 'calibration', 'language': 'en', 'conversation': 'synthetic cal question'},
                            {'id': 'test', 'family_id': 'test', 'split': 'test', 'language': 'en', 'conversation': 'synthetic test question'}]}
        self.bank = {'dataset_sha256': digest(self.dataset)};self.manifest = {'version': 'synthetic-test'}
        self.plan = make_cloud_plan(self.dataset, self.bank, self.manifest);self.sent = [];self.now = 0;self.status = 200
        self.temp = tempfile.TemporaryDirectory();self.path = Path(self.temp.name)/'ledger.jsonl';self.proxy = None

    def tearDown(self):
        if self.proxy:self.proxy.close()
        self.temp.cleanup()

    def response(self, request):
        self.sent.append(request)
        lines = [json.loads(line) for line in self.path.read_text().splitlines()]
        self.assertEqual(len([r for r in lines if r['event'] == 'dispatch']), len(self.sent))
        self.assertNotIn('synthetic-secret', self.path.read_text())
        if self.status != 200:return httpx.Response(self.status, json={'error': 'synthetic-secret and prompt must be redacted'})
        return httpx.Response(200, json={'usage': {'input_tokens': 5, 'output_tokens': 2, 'total_tokens': 7}})

    def start(self, **kwargs):
        options = {'simulation': True, 'transport': httpx.MockTransport(self.response), 'clock': lambda: self.now}
        options.update(kwargs)
        self.proxy = BoundedCloudProxy({'openai': 'synthetic-secret', 'gemini-lite': 'synthetic-secret'}, self.plan,
                                      self.dataset, self.bank, self.manifest, self.path, **options)

    def dispatch(self, name, identity):
        self.proxy.expect(name, identity);case = next(c for c in self.dataset['cases'] if c['id'] == identity)
        handler = Handler(expected_body(name, case), PATHS[name]);self.proxy.forward(handler);return handler

    def test_plan_has_separate_pending_allocation_and_exact_request_order(self):
        self.assertEqual(self.plan['maximum_calls'], 4);self.assertEqual(self.plan['approval_status'], 'pending')
        self.assertEqual([r['case_id'] for r in self.plan['requests']], ['cal', 'test'] * 2)
        self.assertEqual(self.plan['core_temperature']['gemini-lite'], 1)
        self.assertEqual(self.plan['wire_temperature']['openai'], 'provider_default')
        self.assertFalse(self.plan['retry']);self.assertFalse(self.plan['fresh_holdout']);self.assertFalse(self.plan['quality_evidence'])
        changed = deepcopy(self.bank);changed['dataset_sha256'] = '0' * 64
        with self.assertRaises(ValueError):make_cloud_plan(self.dataset, changed, self.manifest)

    def test_default_preflight_has_no_upstream_even_with_a_valid_body(self):
        self.start(simulation=False, transport=None)
        self.assertEqual(self.dispatch('openai', 'cal').status, 403)
        self.assertEqual(self.sent, []);self.assertEqual(self.proxy.records, [])

    def test_mock_mode_cannot_be_mixed_with_live_or_real_transport(self):
        for options in ({'simulation': True, 'execute': True, 'transport': httpx.MockTransport(self.response)},
                        {'simulation': True, 'transport': httpx.HTTPTransport()},
                        {'execute': True, 'transport': httpx.MockTransport(self.response)}):
            try:
                with self.assertRaises(ValueError):self.start(**options)
            finally:
                if isinstance(options.get('transport'), httpx.HTTPTransport):options['transport'].close()

    def test_all_slots_are_durable_before_egress_and_duplicates_cannot_retry(self):
        self.start()
        for name in MODELS:
            for case in self.dataset['cases']:
                self.now += 10;self.assertEqual(self.dispatch(name, case['id']).status, 200)
        self.assertEqual(len(self.sent), 4)
        self.assertEqual({str(r.url) for r in self.sent}, set(URLS.values()))
        self.assertTrue(all(r['source'] == 'synthetic' for r in self.proxy.records))
        with self.assertRaises(ValueError):self.proxy.expect('openai', 'cal')
        self.assertEqual(len(self.sent), 4)
        text = self.path.read_text()
        self.assertNotIn('synthetic cal question', text);self.assertNotIn('Authorization', text)

    def test_prompt_sampling_tools_effort_and_destination_mutations_do_not_escape(self):
        self.start()
        for name in MODELS:
            case = self.dataset['cases'][0];expected = expected_body(name, case)
            changes = [lambda b: b.update(tools=[]), lambda b: b.update(systemInstruction={'parts':[{'text':'changed'}]}),
                       lambda b: b.update(extra='changed')]
            if name == 'openai':
                changes += [lambda b: b.update(model='different'), lambda b: b.update(reasoning={'effort':'high'}),
                            lambda b: b.update(max_output_tokens=513), lambda b: b['input'][0].update(content='changed')]
            else:
                changes += [lambda b: b['generationConfig'].update(temperature=0), lambda b: b['generationConfig'].update(maxOutputTokens=513),
                            lambda b: b['contents'][0]['parts'][0].update(text='changed')]
            for mutation in changes:
                body = deepcopy(expected);mutation(body);self.proxy.expect(name, case['id'])
                handler = Handler(body, PATHS[name]);self.proxy.forward(handler);self.assertEqual(handler.status, 403)
            self.proxy.expect(name, case['id']);handler = Handler(expected, '/unapproved');self.proxy.forward(handler)
            self.assertEqual(handler.status, 403)
        self.assertEqual(self.sent, []);self.assertEqual(self.proxy.records, [])

    def test_caps_deadline_spacing_and_failed_spend_are_not_refunded(self):
        self.start();self.assertEqual(self.dispatch('gemini-lite', 'cal').status, 200)
        self.assertEqual(self.dispatch('gemini-lite', 'test').status, 429);self.assertEqual(len(self.sent), 1)
        self.now = 10;self.assertEqual(self.dispatch('gemini-lite', 'test').status, 200)
        self.proxy.tokens['openai'] = 50000
        self.assertEqual(self.dispatch('openai', 'cal').status, 429)
        self.proxy.tokens['openai'] = 0;self.now = 900
        self.assertEqual(self.dispatch('openai', 'cal').status, 429)
        self.assertEqual(len(self.sent), 2)

    def test_auth_or_quota_failure_blocks_following_provider_attempts(self):
        self.start();self.status = 429
        self.assertEqual(self.dispatch('openai', 'cal').status, 429)
        self.assertEqual(self.proxy.counts['openai'], 1);self.assertGreater(self.proxy.tokens['openai'], 0)
        self.assertEqual(self.proxy.records[0]['status'], 'failed_usage_unknown')
        self.assertEqual(self.dispatch('openai', 'test').status, 429);self.assertEqual(len(self.sent), 1)
        with self.assertRaises(ValueError):self.proxy.expect('openai', 'cal')
        self.assertNotIn('synthetic-secret', self.path.read_text())

    def test_modified_plan_and_existing_ledger_are_refused(self):
        changed = deepcopy(self.plan);changed['maximum_calls'] += 1
        with self.assertRaises(ValueError):BoundedCloudProxy({}, changed, self.dataset, self.bank, self.manifest, self.path)
        self.start()
        with self.assertRaises(FileExistsError):BoundedCloudProxy({}, self.plan, self.dataset, self.bank, self.manifest, self.path)


if __name__ == '__main__':unittest.main()
