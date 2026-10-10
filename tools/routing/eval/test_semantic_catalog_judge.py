import copy
import unittest

from judge_review import digest
import semantic_catalog_judge as catalog
import semantic_judge as v1
from semantic_review import prepare
import test_semantic_review as review_fixtures


def fixture():
    bundle = prepare(*review_fixtures.SemanticReviewTests().fixture(), 'baseline', 'retrospective')
    return bundle, catalog.make_plan(bundle[0], bundle[2], ['0', '1'])


def assessment(item, outcome='pass'):
    return {'ratings': [{'criterion_id': c['id'], 'outcome': outcome,
                         'rationale': 'Synthetic catalogue contract fixture only.',
                         'evidence_ids': [catalog.catalogue(item)[0]['id']] if outcome != 'unknown' else []}
                        for c in item['criteria']]}


class SemanticCatalogTests(unittest.TestCase):
    def test_exact_text_slices_and_deterministic_ids(self):
        bundle, _ = fixture(); item = bundle[0]['items'][0]
        item['response'] = '  alpha\nβeta ' * 150
        items = catalog.catalogue(item)
        self.assertEqual(items, catalog.catalogue(item))
        self.assertEqual(len({i['id'] for i in items}), len(items))
        for entry in items:
            original = item['prompt'] if entry['source'] == 'question' else item['response']
            self.assertIn(entry['quote'], original); self.assertLessEqual(len(entry['quote']), 400)

    def test_native_schema_requires_ids_and_omits_output_reference_index(self):
        bundle, plan = fixture(); item = bundle[0]['items'][0]
        catalog.validate_plan(bundle[0], bundle[2], plan)
        for reviewer in catalog.MODELS:
            value = catalog.body(reviewer, item)
            schema = value['text']['format']['schema'] if reviewer == 'openai' else value['generationConfig']['responseJsonSchema']
            props = schema['properties']['ratings']['items']['properties']
            self.assertIn('evidence_ids', props); self.assertNotIn('reference_index', props)
            self.assertEqual(props['evidence_ids']['items']['enum'], [e['id'] for e in catalog.catalogue(item)])

    def test_ids_convert_to_exact_original_anchors_without_loosening_validation(self):
        bundle, _ = fixture(); item = bundle[0]['items'][0]
        value = catalog.prepare_assessment(item, assessment(item))
        ratings = v1.validate_assessment(item, value)
        self.assertTrue(all(r['outcome'] == 'pass' for r in ratings))
        self.assertEqual(value['ratings'][0]['evidence'][0]['quote'], catalog.catalogue(item)[0]['quote'])

    def test_foreign_duplicate_ids_missing_criteria_and_unanchored_pass_refused(self):
        bundle, _ = fixture(); item = bundle[0]['items'][0]
        for change in (lambda x: x['ratings'][0].update(evidence_ids=['foreign']),
                       lambda x: x['ratings'][0].update(evidence_ids=['e0', 'e0']),
                       lambda x: x['ratings'][0].update(evidence_ids=[]), lambda x: x['ratings'].pop()):
            value = assessment(item); change(value)
            with self.assertRaises(ValueError): catalog.prepare_assessment(item, value)
        catalog.prepare_assessment(item, assessment(item, 'unknown'))

    def test_new_plan_cannot_be_validated_as_old_protocol_or_tampered(self):
        bundle, plan = fixture()
        with self.assertRaises(ValueError): v1.validate_plan(bundle[0], bundle[2], plan)
        old = v1.make_plan(bundle[0], bundle[2], ['0', '1'])
        with self.assertRaises(ValueError): catalog.validate_plan(bundle[0], bundle[2], old)
        changed = copy.deepcopy(plan); changed['requests'][0]['body_sha256'] = '0' * 64
        with self.assertRaises(ValueError): catalog.validate_plan(bundle[0], bundle[2], changed)

    def test_consensus_and_full_denominator_preserve_actual_new_plan_binding(self):
        bundle, plan = fixture(); items = {i['blind_id']: i for i in bundle[0]['items']}
        rows = [{'reviewer': s['reviewer'], 'blind_id': s['blind_id'], 'body_sha256': s['body_sha256'],
                 'status': 'completed', 'assessment': catalog.prepare_assessment(items[s['blind_id']], assessment(items[s['blind_id']]))}
                for s in plan['requests']]
        _, result = catalog.diagnostics(*bundle[:3], plan, rows)
        self.assertEqual(result['plan_sha256'], digest(plan)); self.assertEqual(result['observations_sha256'], digest(rows))
        self.assertTrue(all(g['pass'] == 2 for g in result['sample_groups']))
        self.assertFalse(result['p12_eligible']); self.assertFalse(result['independent_ground_truth'])
        for row in rows:
            if row['reviewer'] == 'gemini-lite':
                for rating in row['assessment']['ratings']: rating['outcome'] = 'fail'
        _, result = catalog.diagnostics(*bundle[:3], plan, rows)
        self.assertTrue(all(g['unknown'] == 2 for g in result['sample_groups'] if g['reviewer'] == 'consensus'))
        with self.assertRaises(ValueError): catalog.diagnostics(*bundle[:3], plan, rows + rows[:1])


if __name__ == '__main__': unittest.main()
