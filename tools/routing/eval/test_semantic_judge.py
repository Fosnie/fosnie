import copy
import json
import unittest

from judge_review import digest
from semantic_judge import (MODELS, body, diagnostics, extract, make_plan, parse_json,
                            validate_assessment, validate_plan)
from semantic_review import prepare
import test_semantic_review as review_fixtures


def fixture():
    bundle = prepare(*review_fixtures.SemanticReviewTests().fixture(), 'baseline', 'retrospective')
    return bundle, make_plan(bundle[0], bundle[2], ['0', '1'])


def assessment(item, outcome='pass'):
    return {'ratings': [{'criterion_id': c['id'], 'outcome': outcome,
                         'rationale': 'Synthetic supplied answer covers the fixture.',
                         'evidence': [{'source': 'answer', 'reference_index': None, 'quote': item['response']}]
                         if outcome != 'unknown' else []} for c in item['criteria']]}


def native(reviewer, value):
    content = json.dumps(value)
    if reviewer == 'openai':
        return {'status': 'completed', 'model': MODELS[reviewer],
                'output': [{'type': 'message', 'status': 'completed', 'role': 'assistant',
                            'content': [{'type': 'output_text', 'text': content}]}],
                'usage': {'input_tokens': 10, 'output_tokens': 20, 'total_tokens': 30}}
    return {'modelVersion': MODELS[reviewer], 'candidates': [{'finishReason': 'STOP',
            'content': {'role': 'model', 'parts': [{'text': 'private synthetic reasoning', 'thought': True}, {'text': content}]}}],
            'usageMetadata': {'promptTokenCount': 10, 'candidatesTokenCount': 20, 'totalTokenCount': 30}}


class SemanticJudgeTests(unittest.TestCase):
    def test_fixed_cohort_plan_and_metadata_projection(self):
        bundle, plan = fixture(); public, private, manifest, _ = bundle
        validate_plan(public, manifest, plan)
        self.assertEqual(plan['maximum_calls'], 8)
        self.assertEqual(set(plan['reviewers']), set(MODELS))
        self.assertFalse(plan['retry']); self.assertFalse(plan['p12_eligible'])
        for name in MODELS:
            value = body(name, public['items'][0]); wire = json.dumps(value)
            for forbidden in ('NEVER_IN_PAGE', 'profile_sha256', 'strategy', 'baseline'):
                self.assertNotIn(forbidden, json.dumps(json.loads(value['input'][1]['content']) if name == 'openai' else json.loads(value['contents'][0]['parts'][0]['text'])))
            self.assertNotIn('NEVER_IN_PAGE', wire)

    def test_changed_cap_destination_or_duplicate_case_refused(self):
        bundle, plan = fixture()
        for change in (lambda p: p.update(maximum_calls=999), lambda p: p['destinations'].update(openai='https://example.invalid'),
                       lambda p: p['case_ids'].append('0')):
            changed = copy.deepcopy(plan); change(changed)
            with self.assertRaises(ValueError): validate_plan(bundle[0], bundle[2], changed)

    def test_both_native_extractions_skip_thought_and_require_terminal(self):
        bundle, _ = fixture(); value = assessment(bundle[0]['items'][0])
        for reviewer in MODELS:
            data = native(reviewer, value)
            self.assertEqual(extract(reviewer, data), value)
            if reviewer == 'openai': data['status'] = 'incomplete'
            else: data['candidates'][0]['finishReason'] = 'MAX_TOKENS'
            with self.assertRaisesRegex(ValueError, 'incomplete'): extract(reviewer, data)

    def test_native_refusals_and_tool_parts_never_scored(self):
        value = native('openai', {})
        value['output'][0]['content'] = [{'type': 'refusal', 'refusal': 'refused'}]
        with self.assertRaises(ValueError): extract('openai', value)
        value = native('gemini-lite', {})
        value['candidates'][0]['content']['parts'] = [{'functionCall': {'name': 'unsafe'}}]
        with self.assertRaises(ValueError): extract('gemini-lite', value)

    def test_quotes_must_be_exact_and_non_unknown_ratings_anchored(self):
        bundle, _ = fixture(); item = bundle[0]['items'][0]
        value = assessment(item); validate_assessment(item, value)
        value['ratings'][0]['evidence'][0]['quote'] = 'invented quote'
        with self.assertRaisesRegex(ValueError, 'fabricated'): validate_assessment(item, value)
        value = assessment(item); value['ratings'][0]['evidence'] = []
        with self.assertRaisesRegex(ValueError, 'unanchored'): validate_assessment(item, value)

    def test_reference_index_and_every_criterion_required(self):
        bundle, _ = fixture(); item = bundle[0]['items'][0]; value = assessment(item)
        value['ratings'][0]['evidence'][0].update(source='reference', reference_index=0)
        with self.assertRaisesRegex(ValueError, 'reference'): validate_assessment(item, value)
        value = assessment(item); value['ratings'][0]['criterion_id'] = value['ratings'][1]['criterion_id']
        with self.assertRaisesRegex(ValueError, 'duplicate'): validate_assessment(item, value)
        value['ratings'].pop()
        with self.assertRaisesRegex(ValueError, 'every'): validate_assessment(item, value)

    def test_disagreement_unknown_and_missing_reviewer_never_becomes_pass(self):
        bundle, plan = fixture(); items = {i['blind_id']: i for i in bundle[0]['items']}
        observations = []
        for slot in plan['requests']:
            observations.append({'reviewer': slot['reviewer'], 'blind_id': slot['blind_id'],
                'body_sha256': slot['body_sha256'], 'status': 'completed',
                'assessment': assessment(items[slot['blind_id']], 'pass' if slot['reviewer'] == 'openai' else 'fail')})
        reviews, result = diagnostics(*bundle[:3], plan, observations)
        sampled = [g for g in result['sample_groups'] if g['reviewer'] == 'consensus']
        self.assertTrue(all(g['unknown'] == 2 for g in sampled))
        self.assertFalse(result['p12_eligible']); self.assertFalse(result['real_human_review'])
        self.assertEqual(next(c['count'] for c in result['criterion_agreement'] if c['openai'] == 'pass' and c['gemini-lite'] == 'fail'), 12)
        only_one = [o for o in observations if o['reviewer'] == 'openai']
        _, result = diagnostics(*bundle[:3], plan, only_one)
        self.assertTrue(all(g['pass'] == 0 for g in result['sample_groups'] if g['reviewer'] == 'consensus'))

    def test_agreement_scores_sample_only_and_preserves_full_denominator(self):
        bundle, plan = fixture(); items = {i['blind_id']: i for i in bundle[0]['items']}
        observations = [{'reviewer': s['reviewer'], 'blind_id': s['blind_id'], 'body_sha256': s['body_sha256'],
                         'status': 'completed', 'assessment': assessment(items[s['blind_id']])} for s in plan['requests']]
        reviews, result = diagnostics(*bundle[:3], plan, observations, grading_source='live')
        self.assertEqual(result['grading_source'], 'live')
        self.assertFalse(result['full_cohort_semantically_assessed'])
        self.assertEqual(sum(g['unknown'] for g in result['full_cohort_reports']['consensus']['groups']), 2)
        self.assertTrue(all(g['pass'] == 2 for g in result['sample_groups']))
        self.assertEqual(reviews['consensus']['reviewer']['kind'], 'machine')

    def test_duplicate_foreign_or_changed_observation_refused(self):
        bundle, plan = fixture(); slot = plan['requests'][0]
        row = {'reviewer': slot['reviewer'], 'blind_id': slot['blind_id'], 'body_sha256': slot['body_sha256'],
               'status': 'unknown', 'assessment': None}
        for rows in ([row, row], [{**row, 'blind_id': 'foreign'}], [{**row, 'body_sha256': '0' * 64}],
                     [{**row, 'assessment': {}}]):
            with self.assertRaises(ValueError): diagnostics(*bundle[:3], plan, rows)

    def test_duplicate_json_nan_and_unavailable_input_refused(self):
        for raw in ('{"ratings":[],"ratings":[]}', '{"x":NaN}'):
            with self.assertRaises(ValueError): parse_json(raw)
        bundle, _ = fixture(); item = copy.deepcopy(bundle[0]['items'][0]); item['available'] = False
        with self.assertRaises(ValueError): body('openai', item)


if __name__ == '__main__': unittest.main()
