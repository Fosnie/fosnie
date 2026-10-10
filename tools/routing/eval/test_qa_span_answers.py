import copy
import hashlib
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

from judge_review import digest
import qa_span_answers as spans
from qa_span_corpus import build
from qa_span_pilot import execute, STRATEGIES


def fixture():
    articles = []
    for i in range(25):
        context = f'This passage states that the city is Oslo{i}. It also mentions Paris as an unrelated place.'
        answer = 'Oslo' + str(i)
        articles.append({'title': 'Synthetic article ' + str(i), 'paragraphs': [{'context': context, 'qas': [
            {'id': 'q' + str(i), 'question': 'Which city does this passage identify?', 'answers': [{'text': answer, 'answer_start': context.index(answer)}]}]}]})
    document = {'version': '1.1', 'data': articles}; source = {'fixture': 'synthetic contract only'}
    exposure = {'schema_version': 1, 'case_ids': [], 'family_ids': [], 'files': []}
    return build(document, source, exposure), document, source, exposure


class SpanAnswersTests(unittest.TestCase):
    def test_normalised_match_preserves_order_and_does_not_search_rationale(self):
        ref = {'answers': [{'text': 'The New York', 'answer_start': 0}]}
        self.assertEqual(spans.assess(ref, 'Reasoning omitted.\nFinal answer: "NEW YORK"')['verdict'], 'pass')
        self.assertEqual(spans.assess(ref, 'New York is mentioned, but the answer is elsewhere.\nFinal answer: Paris')['verdict'], 'fail')
        self.assertNotEqual(spans.normalize('dog bites man'), spans.normalize('man bites dog'))

    def test_bold_labels_full_bold_lines_and_values_are_distinguished(self):
        ref = {'answers': [{'text': 'Oslo'}]}
        for content in ('**Final answer:** Oslo', '**Final answer: Oslo**', 'Final answer: **Oslo**', 'Final answer: `Oslo`'):
            self.assertEqual(spans.assess(ref, content)['verdict'], 'pass')
        self.assertFalse(spans.assess(ref, 'Oslo')['format_marker_compliant'])

    def test_multiple_nonterminal_empty_or_incomplete_answers_unknown(self):
        ref = {'answers': [{'text': 'Oslo'}]}
        for content in ('Final answer: Paris\nFinal answer: Oslo', 'Final answer: Oslo\nAnother line.', '', 'Final answer: the'):
            self.assertEqual(spans.assess(ref, content)['verdict'], 'unknown')
        for kwargs in ({'finish_reason': 'length'}, {'target_matches': False}, {'contract_status': 'failed'}):
            self.assertEqual(spans.assess(ref, 'Final answer: Oslo', **kwargs)['verdict'], 'unknown')

    def test_exact_annotation_offsets_and_source_prompt_bindings(self):
        (dataset, bank, _), *_ = fixture(); spans.validate(dataset, bank)
        for change in (lambda b: b['cases'][0]['answers'][0].update(answer_start=0),
                       lambda b: b['cases'][0].update(context='Changed passage'),
                       lambda b: b.update(evaluator_sha256='0' * 64)):
            other = copy.deepcopy(bank); change(other)
            with self.assertRaises(ValueError): spans.validate(dataset, other)

    def test_document_group_exposure_excluded_without_quality_admission(self):
        (dataset, bank, manifest), document, source, exposure = fixture()
        excluded = dataset['cases'][0]['family_id']; exposure['family_ids'] = [excluded]
        fresh, _, status = build(document, source, exposure)
        self.assertNotIn(excluded, {c['family_id'] for c in fresh['cases']})
        self.assertEqual(len({c['family_id'] for c in fresh['cases']}), 20)
        self.assertFalse(status['family_independence_reviewed']); self.assertEqual(status['upstream_split'], 'train')

    def test_denominator_and_duplicate_rows_and_marker_are_separate(self):
        (dataset, bank, _), *_ = fixture(); row = {'case_id': dataset['cases'][0]['id'], 'strategy': 'gemini-lite', 'verdict': 'pass'}
        result = spans.summarise(dataset, bank, [row], STRATEGIES)
        gemini = next(g for g in result['groups'] if g['strategy'] == 'gemini-lite')
        self.assertEqual((gemini['pass'], gemini['unknown']), (1, 19)); self.assertFalse(result['p12_eligible'])
        with self.assertRaises(ValueError): spans.summarise(dataset, bank, [row, row], STRATEGIES)

    def test_five_core_strategies_share_workflow_intent_not_gold_hints(self):
        (dataset, bank, _), *_ = fixture()
        plan = {'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank), 'evaluator_sha256': spans.fingerprint(),
            'strategies': list(STRATEGIES), 'maximum_primary_calls': 100, 'maximum_generation_calls': 140,
            'baseline': 'gemini-lite', 'maximum_reserved_cost_microusd': 5000000, 'run_duration_seconds': 3600}
        values = {r['case_id']: r['answers'][0]['text'] for r in bank['cases']}
        class Client:
            def __init__(self): self.calls = []
            def request(self, path, body, timeout): self.calls.append(body); return {'content': 'Final answer: ' + values[body['case_id']]}
        class Guard:
            source = 'synthetic'; rejections = []
            def expect(self, *args): pass
            def clear(self): pass
            def halted(self): return False
        def payload(case, selection, runtime):
            return {'case_id': case['id'], 'evaluation_id': selection['id'] + case['id'], 'requirements': {'task': 'unknown'},
                'messages': [{'role': 'user', 'content': case['conversation']}], 'max_cost_microusd': 50000, 'strategy': selection['id']}
        def score(value, *args):
            return {'contract_status': 'completed', 'finish_reason': 'stop', 'target_matches': True, 'answer_sha256': hashlib.sha256(value['content'].encode()).hexdigest()}
        config = {'runtime': {}, 'selections': {n: {'id': n} for n in STRATEGIES}}; client = Client()
        with tempfile.TemporaryDirectory() as directory, patch('qa_span_pilot.preflight'), patch('qa_span_pilot.payload', side_effect=payload), patch('qa_span_pilot.score', side_effect=score):
            result = execute(dataset, bank, config, plan, Path(directory) / 'run', client, Guard())
        self.assertTrue(result['complete']); self.assertEqual(len(client.calls), 100)
        self.assertEqual(sum(c['strategy'] == 'gemini-lite' for c in client.calls), 20)
        self.assertTrue(all(c['requirements']['task'] == 'extraction' for c in client.calls))
        self.assertTrue(all(r['verdict'] == 'pass' for r in result['records']))


if __name__ == '__main__': unittest.main()
