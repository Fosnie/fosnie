from copy import deepcopy
import json
from pathlib import Path
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

from audited_run import (CONTRACT, CoreClient, NoRedirect, aggregate, execute, main, make_plan,
                         manual_observations, payload, preflight, read_config, score)
from cohort_report import summarise
from judge_review import digest, write_new


PROVIDER = '00000000-0000-0000-0000-000000000001'
JUDGE = '00000000-0000-0000-0000-000000000002'
POLICY = '00000000-0000-0000-0000-000000000003'


class FakeCore:
    def __init__(self, config, targets):
        self.config = deepcopy(config);self.targets = targets;self.posts = [];self.gets = 0
        self.fault = None;self.drift_after = None;self.before_post = None

    def request(self, path, body=None, timeout=5):
        if body is None:
            self.gets += 1
            if path.endswith('management'):return {'deployment_manage': True}
            if path.endswith('policies'):return deepcopy(self.config['policies'])
            entries = [{'id': p['snapshot']['provider_id'], 'enabled': True,
                        'provider_revision': p['snapshot']['provider_revision'], 'profile_revision': p['snapshot']['revision'],
                        'profile': deepcopy(p['snapshot'])} for p in self.config['profiles']]
            if self.drift_after is not None and len(self.posts) >= self.drift_after:entries[0]['profile']['model'] = 'changed'
            return entries
        if self.before_post:self.before_post(body)
        self.posts.append(deepcopy(body))
        if self.fault == 'transport':raise urllib.error.URLError('SECRET provider exception and prompt')
        profile = self.config['profiles'][0];saved = profile['snapshot']
        actual = {'provider_id': PROVIDER, 'provider_revision': saved['provider_revision'], 'profile_revision': saved['revision'],
                  'model': saved['model'], 'effort': body['requirements'].get('effort')}
        calls = 2 if body['selection']['mode'] == 'auto' else 1
        ledger = {'calls': calls, 'cost_microusd': 10, 'tokens': 20, 'unknown_cost_calls': 0, 'unknown_token_calls': 0}
        result = {'ok': True, 'mode': 'live', 'evaluation_id': body['evaluation_id'], 'content': self.targets[body['messages'][0]['content']],
                  'contract_version': CONTRACT, 'finish_reason': 'stop', 'profile_sha256': profile['profile_sha256'],
                  'automatic_replay': False, 'quality_evaluated': False,
                  'decision': {'policy_id': POLICY, 'policy_revision': 1, 'strategy': 'hybrid', 'actual': actual},
                  'usage': ledger, 'classifier_ledger': {**ledger, 'calls': calls - 1},
                  'stage_usage': [{'purpose': 'primary', 'provider_id': PROVIDER, 'input_tokens': 10, 'output_tokens': 10, 'total_tokens': 20}]}
        if self.fault == 'core_failure':return {'ok': False, 'mode': 'live', 'evaluation_id': body['evaluation_id'], 'automatic_replay': False}
        return result


class AuditedRunTest(unittest.TestCase):
    def setUp(self):
        self.dataset = {'schema_version': 1, 'dataset_id': 'audited-synthetic-fixture',
                        'source': {'kind': 'synthetic', 'reference': 'contract fixture; no quality claim'}, 'cases': [
                            {'id': 'cal', 'family_id': 'cal-family', 'split': 'calibration', 'language': 'en', 'conversation': 'Private prompt one'},
                            {'id': 'test1', 'family_id': 'test-family', 'split': 'test', 'language': 'en', 'conversation': 'Private prompt two'},
                            {'id': 'test2', 'family_id': 'test-family', 'split': 'test', 'language': 'en', 'conversation': 'Private prompt three'}]}
        self.bank = {'cases': [{'case_id': c['id'], 'target': 'True'} for c in self.dataset['cases']]};self.manifest = {'version': 'test'}
        profile = {'provider_id': PROVIDER, 'provider_revision': 1, 'revision': 2, 'adapter': 'ollama', 'model': 'synthetic-model',
                   'capabilities': {}, 'locality': 'local', 'locality_trusted': True}
        policy = {'id': POLICY, 'revision': 1, 'scope': 'user', 'is_default': True,
                  'policy': {'id': POLICY, 'revision': 1, 'strategy': 'hybrid', 'rollout': 'active', 'classifier_provider_id': JUDGE}}
        self.config = {'schema_version': 1, 'dataset_sha256': digest(self.dataset), 'base_url': 'http://127.0.0.1:9',
                       'auth_env': 'FOSNIE_ROUTING_EVAL_TOKEN', 'local_only': True,
                       'limits': {'max_requests': 6, 'max_model_calls': 9, 'total_budget_microusd': 600,
                                  'request_budget_microusd': 100, 'max_duration_ms': 100, 'run_duration_ms': 30000, 'max_output_tokens': 512},
                       'case_ids': ['cal', 'test1', 'test2'],
                       'profiles': [{'snapshot': profile, 'profile_sha256': 'a' * 64},
                                    {'snapshot': {**profile, 'provider_id': JUDGE}, 'profile_sha256': 'b' * 64}],
                       'policies': [policy], 'configurations': [
                           {'id': 'manual', 'mode': 'manual', 'policy_id': POLICY, 'provider_ids': [PROVIDER], 'efforts': [None], 'objective': 'balanced'},
                           {'id': 'auto', 'mode': 'auto', 'policy_id': POLICY, 'provider_ids': [PROVIDER], 'efforts': [None], 'objective': 'balanced'}]}
        self.targets = {c['conversation']: 'True' for c in self.dataset['cases']}

    def run_config(self, config=None):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'config.json';write_new(path, config or self.config)
            return read_config(path)

    def plan(self, config=None):
        return make_plan(self.dataset, self.bank, self.manifest, config or self.config)

    def test_plan_covers_full_order_and_conservatively_counts_judge(self):
        self.assertEqual(self.run_config(), self.config)
        plan = self.plan()
        self.assertEqual((plan['planned_requests'], plan['maximum_model_calls'], plan['reserved_cost_upper_microusd']), (6, 9, 600))
        self.assertEqual([r['case_id'] for r in plan['requests']], ['cal', 'test1', 'test2'] * 2)
        self.assertFalse(plan['retry']);self.assertFalse(plan['quality_evidence']);self.assertEqual(plan['network_calls'], 0)
        for key, number in [('max_requests', 5), ('max_model_calls', 8), ('total_budget_microusd', 599)]:
            config = deepcopy(self.config);config['limits'][key] = number
            with self.subTest(key=key), self.assertRaises(ValueError):self.plan(config)

    def test_invalid_limits_destinations_locality_and_duplicate_cases_refuse(self):
        changes = [lambda c: c['limits'].update(max_requests=True), lambda c: c.update(base_url='http://cloud.example'),
                   lambda c: c.update(base_url='https://user:secret@cloud.example'), lambda c: c.update(base_url='https://core.example?key=x'),
                   lambda c: c.update(base_url='https://core.example/path'), lambda c: c.update(auth_env='TOKEN\nSECRET'),
                   lambda c: c['profiles'][1]['snapshot'].update(locality='cloud'), lambda c: c['profiles'][0]['snapshot'].update(key='SECRET'),
                   lambda c: c['case_ids'].append('cal'), lambda c: c['policies'][0]['policy'].update(classifier_provider_id='absent')]
        for change in changes:
            config = deepcopy(self.config);change(config)
            with self.subTest(change=change), self.assertRaises(ValueError):self.run_config(config)
        config = deepcopy(self.config);config['case_ids'][0] = 'foreign'
        with self.assertRaises(ValueError):self.plan(config)
        config = deepcopy(self.config);config['dataset_sha256'] = '0' * 64
        with self.assertRaises(ValueError):self.plan(config)

    def test_json_effort_profile_snapshot_preserves_empty_denial_and_refuses_invalid_values(self):
        for allowed in (None, [], ['high']):
            config=deepcopy(self.config)
            config['profiles'][0]['snapshot']['capabilities']={'efforts':['none','high'],'structured_output_efforts':allowed}
            self.assertEqual(self.run_config(config),config)
        for allowed in ('high', ['high','high'], ['unknown'], [' high'], [True], ['x'*25]):
            config=deepcopy(self.config)
            config['profiles'][0]['snapshot']['capabilities']={'efforts':['none','high'],'structured_output_efforts':allowed}
            with self.subTest(allowed=allowed),self.assertRaises(ValueError):self.run_config(config)

    def test_explicit_temperature_is_finite_bound_and_part_of_the_plan(self):
        config = deepcopy(self.config);config['configurations'][0]['temperature'] = 1.0
        self.assertEqual(self.run_config(config), config)
        request = payload(self.dataset['cases'][0], config['configurations'][0], config)
        self.assertEqual(request['temperature'], 1.0)
        self.assertNotEqual(self.plan(config)['configuration_sha256'], self.plan()['configuration_sha256'])
        for value in (True, -0.1, 2.1, '1'):
            changed = deepcopy(config);changed['configurations'][0]['temperature'] = value
            with self.subTest(value=value), self.assertRaises(ValueError):self.run_config(changed)

    def test_cli_plan_is_zero_network_and_old_plan_or_output_cannot_execute(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp);config = root / 'config.json';write_new(config, self.config)
            args = ['audited_run', 'plan', '--cohort', str(root / 'cohort'), '--parent', str(root / 'parent'),
                    '--config', str(config), '--output', str(root / 'plan.json')]
            with patch('audited_run.check', return_value=(self.dataset, self.bank, self.manifest, [])), patch('audited_run.CoreClient') as network:
                with patch('sys.argv', args):self.assertEqual(main(), 0)
                network.assert_not_called()
                with patch('sys.argv', args), self.assertRaises(ValueError):main()
                wrong = self.plan();wrong['dataset_sha256'] = '0' * 64;write_new(root / 'wrong.json', wrong)
                args[1] = 'run';args[-1] = str(root / 'run');args += ['--plan', str(root / 'wrong.json'), '--execute']
                with patch('sys.argv', args), self.assertRaises(ValueError):main()
                args[-2] = str(root / 'plan.json');args.pop()
                with patch('sys.argv', args), self.assertRaises(ValueError):main()
                network.assert_not_called()

    def test_successful_manual_and_auto_keep_native_targets_and_no_response_text(self):
        core = FakeCore(self.config, self.targets)
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'run'
            def before_post(body):
                rows = [json.loads(line) for line in (output / 'reservations.jsonl').read_text().splitlines()]
                self.assertEqual(len(rows), len(core.posts) + 1)
                self.assertEqual(rows[-1]['request_sha256'], digest(body))
            core.before_post = before_post
            report = execute(self.dataset, self.bank, self.config, self.plan(), output, core)
            self.assertTrue(report['complete']);self.assertEqual(len(core.posts), 6)
            self.assertEqual(report['reserved_model_calls_upper'], 9)
            self.assertTrue(all(r['verdict'] == 'pass' for r in report['records']))
            self.assertEqual(report['records'][-1]['classifier_ledger']['calls'], 1)
            exported = manual_observations(self.dataset, self.config, report)
            compared = summarise(self.dataset, exported)
            self.assertEqual(compared['groups'][1]['passed_families'], 1)
            text = '\n'.join(p.read_text() for p in output.iterdir())
            for secret in ('Private prompt', 'Authorization', 'Bearer ', 'SECRET'):
                self.assertNotIn(secret, text)
            self.assertFalse(report['quality_evidence']);self.assertIsNone(report['actual_cost_microusd'])
            with self.assertRaises(FileExistsError):execute(self.dataset, self.bank, self.config, self.plan(), output, core)
            self.assertEqual(len(core.posts), 6)

    def test_failed_dispatch_never_refunds_or_retries_and_missing_family_is_unknown(self):
        for fault in ('core_failure', 'transport'):
            core = FakeCore(self.config, self.targets);core.fault = fault
            with tempfile.TemporaryDirectory() as temp:
                report = execute(self.dataset, self.bank, self.config, self.plan(), Path(temp) / 'run', core)
            self.assertEqual(len(core.posts), 6);self.assertEqual(report['reserved_cost_upper_microusd'], 600)
            self.assertTrue(all(r['verdict'] == 'unknown' for r in report['records']))
            self.assertTrue(all(g['passed_families'] == 0 for g in report['groups']))
            self.assertNotIn('SECRET', json.dumps(report))
        groups = aggregate(self.dataset, self.config, [{'configuration_id': 'manual', 'case_id': 'test1', 'verdict': 'pass'}])
        group = next(g for g in groups if g['configuration_id'] == 'manual' and g['split'] == 'test')
        self.assertEqual((group['pass'], group['unknown'], group['passed_families']), (1, 1, 0))

    def test_profile_drift_stops_before_next_dispatch_without_new_reservation(self):
        core = FakeCore(self.config, self.targets);core.drift_after = 1
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'run';report = execute(self.dataset, self.bank, self.config, self.plan(), output, core)
            self.assertEqual(len((output / 'reservations.jsonl').read_text().splitlines()), 1)
        self.assertFalse(report['complete']);self.assertEqual(len(core.posts), 1)
        self.assertEqual(report['reserved_cost_upper_microusd'], 100)
        self.assertEqual(report['stop_reason'], 'preflight_or_persistence_failed')

    def test_auto_cannot_start_with_an_unpinned_possible_executor(self):
        config = deepcopy(self.config);config['profiles'][1]['snapshot']['auto_enabled'] = True
        core = FakeCore(config, self.targets)
        with self.assertRaises(ValueError):preflight(core, config)
        self.assertEqual(len(core.posts), 0)
        config['policies'][0]['policy']['constraints'] = {'allowed_providers': [PROVIDER]}
        core = FakeCore(config, self.targets)
        self.assertEqual(preflight(core, config)['model_calls'], 0)

    def test_terminal_identity_native_fingerprint_and_accounting_are_required(self):
        selection = self.config['configurations'][0];request = payload(self.dataset['cases'][0], selection, self.config)
        value = FakeCore(self.config, self.targets).request('/api/routing/evaluate', request)
        mutations = [lambda v: v.update(finish_reason='length'), lambda v: v.pop('contract_version'),
                     lambda v: v.update(profile_sha256='0' * 64), lambda v: v.update(evaluation_id=POLICY),
                     lambda v: v['decision']['actual'].update(provider_revision=2), lambda v: v['decision']['actual'].update(effort='high'),
                     lambda v: v['decision'].update(policy_revision=2), lambda v: v['usage'].update(calls=2),
                     lambda v: v['usage'].update(cost_microusd=101), lambda v: v['usage'].update(tokens=True),
                     lambda v: v['stage_usage'].append(v['stage_usage'][0]), lambda v: v.update(content='  ')]
        for mutation in mutations:
            changed = deepcopy(value);mutation(changed)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):score(changed, request, selection, self.config, 'True')
        changed = deepcopy(value);changed['content'] = 'True\nExplanation'
        self.assertEqual(score(changed, request, selection, self.config, 'True')['verdict'], 'fail')
        changed['content'] = ' True\n';self.assertEqual(score(changed, request, selection, self.config, 'True')['verdict'], 'pass')

    def test_contract_mismatch_stops_batch_and_deadline_dispatches_nothing(self):
        core = FakeCore(self.config, self.targets);original = core.request
        def missing(path, body=None, timeout=5):
            result = original(path, body, timeout)
            if body is not None:result.pop('finish_reason')
            return result
        core.request = missing
        with tempfile.TemporaryDirectory() as temp:
            report = execute(self.dataset, self.bank, self.config, self.plan(), Path(temp) / 'run', core)
        self.assertEqual(len(core.posts), 1);self.assertEqual(report['records'][0]['verdict'], 'unknown')
        self.assertEqual(report['stop_reason'], 'response_contract_mismatch')
        core = FakeCore(self.config, self.targets)
        times = iter([0, 31])
        with tempfile.TemporaryDirectory() as temp:
            report = execute(self.dataset, self.bank, self.config, self.plan(), Path(temp) / 'run', core, clock=lambda: next(times))
        self.assertEqual(core.gets, 0);self.assertEqual(len(core.posts), 0);self.assertEqual(report['stop_reason'], 'run_deadline')

    def test_http_client_refuses_redirects_bad_credentials_and_duplicate_json(self):
        with self.assertRaises(ValueError):NoRedirect().redirect_request(None, None, None, None, None, None)
        for key in (None, '', 'key\nSECRET'):
            with self.assertRaises(ValueError):CoreClient('http://127.0.0.1:9', key)
        class Response:
            def __enter__(self):return self
            def __exit__(self, *args):pass
            def read(self, bound):return b'{"ok":true,"ok":false}'
        client = CoreClient('http://127.0.0.1:9', 'synthetic-token')
        with patch.object(client.opener, 'open', return_value=Response()), self.assertRaises(ValueError):client.request('/api/routing/management')
        client.close();self.assertIsNone(client.credential)


if __name__ == '__main__':unittest.main()
