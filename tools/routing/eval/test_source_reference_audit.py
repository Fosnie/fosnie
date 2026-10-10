import copy
import unittest

from judge_review import digest
import source_reference_audit as audit


def fixture():
    item = {'blind_id': digest([audit.VERSION, 'squad/fixture']), 'case_id': 'squad/fixture',
        'family_id': 'squad/fixture-family', 'question': 'Which place is named?',
        'context': 'A city named Glasgow. Another city is London.',
        'published_answers': [{'text': 'Glasgow', 'answer_start': 13}]}
    public = {'schema_version': 1, 'protocol': audit.VERSION, 'items': [item]}
    manifest = {'public_sha256': digest(public), 'protocol_sha256': audit.fingerprint()}
    return public, manifest, audit.make_plan(public, manifest)


def assessment(outcome='pass'):
    return {'ratings': [{'criterion_id': c, 'outcome': outcome,
        'rationale': 'Synthetic source-only assessment for testing.', 'evidence_ids': ['c0']} for c in audit.CRITERIA],
        'proposed_spans': []}


class SourceAuditTests(unittest.TestCase):
    def test_payload_and_schema_are_source_only_and_bounded(self):
        public, manifest, plan = fixture(); audit.validate_plan(public, manifest, plan)
        self.assertEqual(plan['maximum_calls'], 2)
        for provider in audit.MODELS:
            body = audit.body(provider, public['items'][0])
            payload = body['input'][1]['content'] if provider == 'openai' else body['contents'][0]['parts'][0]['text']
            for forbidden in ('response', 'strategy', 'profile_sha256', 'family_id', 'blind_id'):
                self.assertNotIn('"' + forbidden + '"', payload)
            schema = audit.schema(public['items'][0])
            self.assertEqual(schema['properties']['ratings']['maxItems'], 4)
            self.assertEqual(schema['properties']['proposed_spans']['maxItems'], 4)

    def test_offsets_tampering_and_duplicate_documents_refused(self):
        public, _, _ = fixture()
        public['items'][0]['published_answers'][0]['answer_start'] = 12
        with self.assertRaisesRegex(ValueError, 'offset'): audit.validate_public(public)
        public, _, _ = fixture(); public['items'].append(copy.deepcopy(public['items'][0]))
        with self.assertRaisesRegex(ValueError, 'duplicate'): audit.validate_public(public)

    def test_literal_presence_does_not_bypass_evidence_or_cardinality(self):
        public, _, _ = fixture(); item = public['items'][0]
        for edit in (lambda v: v['ratings'][0].update(evidence_ids=['r0']),
                     lambda v: v['ratings'][0].update(evidence_ids=['c0', 'c0']),
                     lambda v: v['ratings'].pop(),
                     lambda v: v['ratings'][0].update(criterion_id='injected')):
            value = assessment(); edit(value)
            with self.assertRaises(ValueError): audit.validate_assessment(item, value)

    def test_proposals_exactly_anchored_and_consistent(self):
        public, _, _ = fixture(); item = public['items'][0]; value = assessment('fail')
        value['proposed_spans'] = [{'text': 'London', 'kind': 'missing_alternative', 'rationale': 'Another named city could satisfy this question.'}]
        audit.validate_assessment(item, value)
        for text in ('london', 'Edinburgh', 'Glasgow'):
            changed = copy.deepcopy(value); changed['proposed_spans'][0]['text'] = text
            with self.assertRaises(ValueError): audit.validate_assessment(item, changed)
        value['ratings'][2]['outcome'] = 'pass'
        with self.assertRaisesRegex(ValueError, 'contradicts'): audit.validate_assessment(item, value)

    def test_disagreement_missing_full_denominator_and_no_promotion(self):
        public, manifest, plan = fixture()
        def observations(left, right):
            return [{'reviewer': s['reviewer'], 'blind_id': s['blind_id'], 'body_sha256': s['body_sha256'],
                'status': 'completed', 'assessment': assessment(left if s['reviewer'] == 'openai' else right)} for s in plan['requests']]
        report = audit.diagnostics(public, manifest, plan, observations('pass', 'fail'), source='synthetic')
        self.assertEqual(report['decision_counts']['exclude'], 1)
        self.assertTrue(all(r['consensus'] == 'unknown' for r in report['cases'][0]['criteria']))
        empty = audit.diagnostics(public, manifest, plan, [], source='synthetic')
        self.assertEqual(empty['decision_counts']['unresolved'], 1)
        self.assertEqual(sum(r['count'] for r in empty['criterion_agreement']), 4)
        good = audit.diagnostics(public, manifest, plan, observations('pass', 'pass'), source='synthetic')
        self.assertEqual(good['decision_counts']['machine_preflight_pass'], 1)
        self.assertFalse(good['cases'][0]['executor_generation_ready'])
        self.assertFalse(good['p12_eligible'])

    def test_plan_tamper_and_duplicate_unknown_observation_refused(self):
        public, manifest, plan = fixture(); changed = copy.deepcopy(plan); changed['maximum_calls'] += 1
        with self.assertRaises(ValueError): audit.validate_plan(public, manifest, changed)
        slot = plan['requests'][0]
        row = {k: slot[k] for k in ('reviewer', 'blind_id', 'body_sha256')}
        row.update(status='unknown', assessment=None)
        with self.assertRaisesRegex(ValueError, 'duplicate'): audit.diagnostics(public, manifest, plan, [row, row], source='synthetic')


if __name__ == '__main__': unittest.main()
