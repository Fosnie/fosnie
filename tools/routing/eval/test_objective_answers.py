import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import objective_answers as answers
from objective_corpus import build
from objective_pilot import execute, STRATEGIES
from judge_review import digest
from reference_audit import solve
from public_benchmark import INSTRUCTION


def fixture():
    documents, sources = {}, []
    for task in answers.TASKS:
        examples = []
        for i in range(1, 7):
            question = {
                'boolean_expressions': '(' * i + 'True' + ')' * i + ' is',
                'multistep_arithmetic_two': f'({i} + 1) =',
                'word_sorting': 'Sort the following words alphabetically: List: pear apple ' + 'z' * i,
                'dyck_languages': 'Complete the rest of the sequence, making sure that the parentheses are closed properly. Input: ' + '( ' * i,
                'navigate': f'If you follow these instructions, do you return to the starting point? Take {i} steps forward.',
            }[task]
            target = solve({'family_id': 'bbh/' + task, 'conversation': question + INSTRUCTION})[0]
            examples.append({'input': question, 'target': target})
        documents[task] = json.dumps({'examples': examples}).encode()
        sources.append({'task': task, 'sha256': hashlib.sha256(documents[task]).hexdigest()})
    provenance = {'revision': 'a' * 40, 'sources': sources}
    exposure = {'schema_version': 1, 'case_ids': [], 'original_prompt_sha256': [], 'files': []}
    return build(documents, provenance, exposure), documents, provenance, exposure


class ObjectiveAnswersTests(unittest.TestCase):
    def test_reasoning_text_is_accepted_but_reference_echo_is_not_a_pass(self):
        reference = {'kind': 'integer', 'target': '24'}
        result = answers.assess(reference, 'Working: 12 + 12.\nFinal answer: 24')
        self.assertEqual(result['verdict'], 'pass'); self.assertTrue(result['format_marker_compliant'])
        self.assertEqual(answers.assess(reference, 'The question mentions 24.\nFinal answer: 23')['verdict'], 'fail')
        self.assertEqual(answers.assess(reference, '24 is mentioned, but I do not know the result.')['verdict'], 'unknown')

    def test_bare_terminal_correct_answer_and_marker_compliance_are_separate(self):
        result = answers.assess({'kind': 'boolean', 'target': 'True'}, 'We simplify the expression.\nTRUE')
        self.assertEqual(result['verdict'], 'pass'); self.assertFalse(result['format_marker_compliant'])
        self.assertEqual(answers.assess({'kind': 'integer', 'target': '42'}, 'Final answer: **42**')['verdict'], 'pass')

    def test_multiple_markers_nonterminal_marker_and_truncation_stay_unknown(self):
        ref = {'kind': 'integer', 'target': '2'}
        for content in ('Final answer: 1\nFinal answer: 2', 'Final answer: 2\nThank you.', '', 'Some reasoning.\n2 or 3'):
            self.assertEqual(answers.assess(ref, content)['verdict'], 'unknown')
        for kwargs in ({'finish_reason': 'length'}, {'contract_status': 'failed'}, {'target_matches': False}):
            self.assertEqual(answers.assess(ref, 'Final answer: 2', **kwargs)['verdict'], 'unknown')

    def test_canonical_values_without_eval_or_arbitrary_substring_matching(self):
        for kind, value, expected in (('integer', '+0024.0', '24'), ('integer', '1,000', '1000'),
            ('boolean', 'false.', 'False'), ('yes_no', 'YES', 'Yes'), ('words', 'apple, pear, zebra', 'apple pear zebra'),
            ('brackets', ']} )', '] } )'), ('integer', r'\boxed{42}', '42')):
            self.assertEqual(answers.canonical(kind, value), expected)
        for value in ('__import__("os")', '2 + 2', '4 because I say so', '1,00', '2.5', '9' * 81):
            self.assertIsNone(answers.canonical('integer', value))

    def test_source_target_and_prompt_and_fingerprint_cannot_change(self):
        (dataset, bank, _), *_ = fixture(); answers.validate(dataset, bank)
        for change in (lambda b: b['cases'][0].update(target='wrong'), lambda b: b['cases'][0].update(source_target='wrong'),
                       lambda b: b.update(evaluator_sha256='0' * 64)):
            changed = copy.deepcopy(bank); change(changed)
            with self.assertRaises(ValueError): answers.validate(dataset, changed)
        dataset['cases'][0]['conversation'] += ' changed'; bank['dataset_sha256'] = digest(dataset)
        with self.assertRaises(ValueError): answers.validate(dataset, bank)

    def test_new_selection_excludes_original_case_ids_and_preserves_five_task_groups(self):
        (dataset, bank, manifest), documents, provenance, exposure = fixture()
        old = dataset['cases'][0]['id']; exposure['case_ids'] = [old]
        fresh, _, declaration = build(documents, provenance, exposure)
        self.assertNotIn(old, {c['id'] for c in fresh['cases']})
        self.assertEqual(len(fresh['cases']), 20); self.assertEqual(declaration['source_task_groups'], 5)
        self.assertFalse(declaration['family_independence_reviewed']); self.assertFalse(declaration['p12_eligible'])

    def test_missing_results_keep_denominator_and_duplicate_results_refuse(self):
        (dataset, bank, _), *_ = fixture()
        rows = [{'case_id': dataset['cases'][0]['id'], 'strategy': 'gemini-lite', 'verdict': 'pass'}]
        result = answers.summarise(dataset, bank, rows, STRATEGIES)
        gemini = next(g for g in result['groups'] if g['strategy'] == 'gemini-lite')
        self.assertEqual((gemini['pass'], gemini['unknown']), (1, 19))
        self.assertFalse(result['p12_eligible'])
        with self.assertRaises(ValueError): answers.summarise(dataset, bank, rows + rows, STRATEGIES)

    def test_core_adapter_records_one_baseline_and_no_gold_or_task_hints(self):
        (dataset, bank, _), *_ = fixture()
        plan = {'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank), 'evaluator_sha256': answers.fingerprint(),
            'strategies': list(STRATEGIES), 'maximum_primary_calls': 80, 'maximum_generation_calls': 120,
            'baseline': 'gemini-lite', 'maximum_reserved_cost_microusd': 4000000, 'run_duration_seconds': 3600}
        values = {r['case_id']: r['target'] for r in bank['cases']}
        class Client:
            def __init__(self): self.calls = []
            def request(self, path, body, timeout):
                self.calls.append(body); return {'content': 'Reasoning omitted.\nFinal answer: ' + values[body['case_id']]}
        class Guard:
            source = 'synthetic'; rejections = []
            def expect(self, *args): pass
            def clear(self): pass
            def halted(self): return False
        def payload(case, selection, runtime):
            return {'case_id': case['id'], 'evaluation_id': selection['id'] + case['id'], 'requirements': {'task': 'reasoning'},
                    'max_cost_microusd': 50000, 'messages': [{'role': 'user', 'content': case['conversation']}], 'strategy': selection['id']}
        def attested(value, *args):
            return {'contract_status': 'completed', 'finish_reason': 'stop', 'target_matches': True,
                    'answer_sha256': hashlib.sha256(value['content'].encode()).hexdigest()}
        config = {'runtime': {}, 'selections': {n: {'id': n} for n in STRATEGIES}}; client = Client()
        with tempfile.TemporaryDirectory() as directory, patch('objective_pilot.preflight'), patch('objective_pilot.payload', side_effect=payload), patch('objective_pilot.score', side_effect=attested):
            report = execute(dataset, bank, config, plan, Path(directory) / 'run', client, Guard())
        self.assertTrue(report['complete']); self.assertEqual(len(client.calls), 80)
        self.assertEqual(sum(c['strategy'] == 'gemini-lite' for c in client.calls), 20)
        self.assertEqual(report['reserved_model_calls_upper'], 120)
        self.assertTrue(all(c['requirements']['task'] == 'unknown' for c in client.calls))
        self.assertTrue(all(r['verdict'] == 'pass' for r in report['records']))


if __name__ == '__main__': unittest.main()
