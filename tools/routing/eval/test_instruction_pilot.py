import hashlib
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from instruction_following import evaluator_fingerprint
from instruction_pilot import execute, summarise, STRATEGIES
from judge_review import digest
from semantic_review import propose_rubric

class InstructionPilotTests(unittest.TestCase):
    def dataset(self):
        return {'schema_version': 1, 'dataset_id': 'synthetic-instructions', 'source': {'kind': 'synthetic', 'reference': 'contract fixture'},
                'cases': [{'id': str(i), 'family_id': 'family-' + str(i // 2), 'split': 'test', 'language': 'en', 'conversation': 'Prompt ' + str(i)} for i in range(40)]}

    def test_missing_results_stay_in_denominator_and_baseline_is_not_chosen_on_test(self):
        dataset = self.dataset()
        rows = [{'strategy': 'gemini-lite', 'case_id': '0', 'verdict': 'pass'},
                {'strategy': 'classifier', 'case_id': '0', 'verdict': 'unknown'}]
        result = summarise(dataset, rows)
        baseline = next(row for row in result['groups'] if row['strategy'] == 'gemini-lite')
        self.assertEqual((baseline['pass'], baseline['unknown']), (1, 39))
        self.assertFalse(result['baseline_selected_on_test'])
        self.assertFalse(result['baseline_response_regenerated'])
        self.assertFalse(result['p12_eligible'])
        self.assertEqual(result['paired_test'][0]['baseline_only'], 1)
        with self.assertRaisesRegex(ValueError, 'duplicate'): summarise(dataset, rows + rows[:1])

    def test_bound_inputs_refuse_before_core_or_output_creation(self):
        dataset = self.dataset()
        with tempfile.TemporaryDirectory() as parent:
            output = Path(parent) / 'unused'
            with self.assertRaisesRegex(ValueError, 'frozen'):
                execute(dataset, {}, {}, {'dataset_sha256': '0' * 64}, output, None, None)
            self.assertFalse(output.exists())

    def test_all_five_configurations_share_original_cases_and_one_baseline_generation(self):
        dataset = self.dataset()
        constraints = {case['id']: {'instruction_ids': ['punctuation:no_comma'], 'kwargs': [{}]} for case in dataset['cases']}
        plan = {'dataset_sha256': digest(dataset), 'evaluator_sha256': evaluator_fingerprint(),
                'maximum_generation_calls': 280, 'maximum_primary_calls': 200, 'run_duration_seconds': 7200}
        rubric = propose_rubric(dataset)
        plan['semantic_rubric_sha256'] = digest(rubric)
        config = {'runtime': {}, 'selections': {name: {'id': name} for name in STRATEGIES}}
        class Client:
            def __init__(self): self.calls = []
            def request(self, path, body, timeout):
                self.calls.append(body)
                return {'content': 'text'}
        class Guard:
            source = 'synthetic'
            rejections = []
            def expect(self, *args): pass
            def clear(self): pass
        client = Client()
        def request(case, selection, runtime):
            return {'evaluation_id': selection['id'] + case['id'], 'requirements': {'task': 'reasoning'}, 'max_cost_microusd': 50000,
                    'messages': [{'role': 'user', 'content': case['conversation']}], 'strategy': selection['id']}
        def attested(*args):
            return {'contract_status': 'completed', 'finish_reason': 'stop', 'target_matches': True,
                    'verdict': 'fail', 'answer_sha256': hashlib.sha256(b'text').hexdigest()}
        with tempfile.TemporaryDirectory() as parent, patch('instruction_pilot.preflight'), patch('instruction_pilot.payload', side_effect=request), patch('instruction_pilot.score', side_effect=attested):
            result = execute(dataset, constraints, config, plan, Path(parent) / 'run', client, Guard(), semantic_rubric=rubric)
            self.assertTrue((Path(parent) / 'run/semantic-rubric.json').is_file())
        self.assertTrue(result['complete'])
        self.assertEqual(result['semantic_rubric_sha256'], digest(rubric))
        self.assertEqual(len(client.calls), 200)
        self.assertEqual(sum(row['strategy'] == 'gemini-lite' for row in client.calls), 40)
        self.assertEqual(result['reserved_model_calls_upper'], 280)
        self.assertEqual(result['reserved_cost_upper_microusd'], 10000000)
        self.assertTrue(all(row['requirements']['task'] == 'unknown' for row in client.calls))
        self.assertTrue(all(row['verdict'] == 'pass' for row in result['records']))
        self.assertFalse(result['p12_eligible'])

if __name__ == '__main__': unittest.main()
