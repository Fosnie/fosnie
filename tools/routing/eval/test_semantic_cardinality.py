import copy
import unittest

from judge_review import digest
import semantic_catalog_judge as v2
import semantic_catalog_judge_v3 as v3
import test_semantic_catalog_judge as fixtures


class CardinalityTests(unittest.TestCase):
    def test_both_native_schemas_bound_wire_lists_to_local_contract(self):
        bundle, _ = fixtures.fixture(); item = bundle[0]['items'][0]
        for reviewer in v3.MODELS:
            body = v3.body(reviewer, item)
            schema = body['text']['format']['schema'] if reviewer == 'openai' else body['generationConfig']['responseJsonSchema']
            ratings = schema['properties']['ratings']
            self.assertEqual((ratings['minItems'], ratings['maxItems']), (len(item['criteria']), len(item['criteria'])))
            self.assertEqual(ratings['items']['properties']['evidence_ids']['maxItems'], 4)
        self.assertIn('AT MOST FOUR', v3.SYSTEM)

    def test_old_and_new_plans_are_distinct_and_cannot_substitute(self):
        bundle, old = fixtures.fixture(); new = v3.make_plan(bundle[0], bundle[2], ['0', '1'])
        self.assertNotEqual(new['protocol_sha256'], old['protocol_sha256'])
        self.assertNotEqual(new['requests'][0]['body_sha256'], old['requests'][0]['body_sha256'])
        with self.assertRaises(ValueError): v3.validate_plan(bundle[0], bundle[2], old)
        with self.assertRaises(ValueError): v2.validate_plan(bundle[0], bundle[2], new)

    def test_local_validator_still_rejects_surplus_anchors_without_trimming(self):
        bundle, _ = fixtures.fixture(); item = copy.deepcopy(bundle[0]['items'][0]); item['response'] = 'x' * 2400
        value = fixtures.assessment(item)
        value['ratings'][0]['evidence_ids'] = [e['id'] for e in v3.catalogue(item)][:5]
        with self.assertRaises(ValueError): v3.prepare_assessment(item, value)
        self.assertEqual(len(value['ratings'][0]['evidence_ids']), 5)

    def test_new_aggregation_keeps_bound_observations_and_no_admission(self):
        bundle, _ = fixtures.fixture(); plan = v3.make_plan(bundle[0], bundle[2], ['0', '1'])
        items = {i['blind_id']: i for i in bundle[0]['items']}
        rows = [{'reviewer': s['reviewer'], 'blind_id': s['blind_id'], 'body_sha256': s['body_sha256'], 'status': 'completed',
                 'assessment': v3.prepare_assessment(items[s['blind_id']], fixtures.assessment(items[s['blind_id']]))} for s in plan['requests']]
        _, result = v3.diagnostics(*bundle[:3], plan, rows)
        self.assertEqual(result['observations_sha256'], digest(rows)); self.assertEqual(result['plan_sha256'], digest(plan))
        self.assertFalse(result['quality_evidence']); self.assertFalse(result['p12_eligible'])


if __name__ == '__main__': unittest.main()
