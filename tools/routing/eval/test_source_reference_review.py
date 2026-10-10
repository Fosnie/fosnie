import copy
import hashlib
import unittest

from judge_review import digest
from qa_span_answers import fingerprint, prompt
import source_reference_audit as audit
from source_reference_review import candidates, draft, finalise, page
from test_source_reference_audit import assessment, fixture


def review_fixture():
    public, manifest, plan = fixture(); item = public['items'][0]
    item['family_id'] = 'squad/' + digest('fixture article')
    manifest['public_sha256'] = digest(public); plan = audit.make_plan(public, manifest)
    dataset = {'schema_version': 1, 'dataset_id': 'source-fixture', 'source': {'kind': 'synthetic', 'reference': 'Synthetic source review test only.'},
        'cases': [{'id': item['case_id'], 'family_id': item['family_id'], 'split': 'test', 'language': 'en',
                   'conversation': prompt(item['context'], item['question'])}]}
    bank = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'evaluator_sha256': fingerprint(), 'cases': [
        {'case_id': item['case_id'], 'article': 'Fixture article', 'paragraph_index': 0, 'question': item['question'],
         'context': item['context'], 'answers': item['published_answers'],
         'context_sha256': hashlib.sha256(item['context'].encode()).hexdigest()}]}
    value = assessment('fail'); value['proposed_spans'] = [{'text': 'London', 'kind': 'missing_alternative', 'rationale': 'Another literal source answer for synthetic test.'}]
    observations = [{'reviewer': s['reviewer'], 'blind_id': s['blind_id'], 'body_sha256': s['body_sha256'],
        'status': 'completed', 'assessment': value} for s in plan['requests']]
    report = audit.diagnostics(public, manifest, plan, observations, source='live')
    review = draft(public, report)
    # Test fixture only; no real human attestation or live output is produced.
    review['reviewer'] = {'kind': 'human', 'id': 'synthetic-test-attestation', 'source_only_review_attested': True, 'executor_answers_seen': False}
    review['cases'][0].update(decision='accept', accepted_reference_ids=['published/0', 'proposal/1'], rationale='Source-only synthetic review test fixture.')
    return dataset, bank, public, report, review


class SourceReviewTests(unittest.TestCase):
    def test_machine_or_unattested_draft_cannot_export(self):
        dataset, bank, public, report, review = review_fixture()
        for edit in (lambda r: r['reviewer'].update(kind='machine'), lambda r: r['reviewer'].update(source_only_review_attested=False),
                     lambda r: r['reviewer'].update(executor_answers_seen=True), lambda r: r['reviewer'].update(id='')):
            changed = copy.deepcopy(review); edit(changed)
            with self.assertRaisesRegex(ValueError, 'human'): finalise(dataset, bank, public, report, changed)

    def test_explicit_refs_export_without_changing_originals_or_p12(self):
        values = review_fixture(); original = copy.deepcopy(values[:4])
        dataset, bank, manifest = finalise(*values)
        self.assertEqual(values[:4], tuple(original))
        self.assertEqual([r['text'] for r in bank['cases'][0]['answers']], ['Glasgow', 'London'])
        self.assertFalse(manifest['p12_eligible']); self.assertFalse(manifest['family_independence_reviewed'])
        self.assertEqual(manifest['reference_provenance'][0]['selected_references'][1]['origin'], 'machine_proposed')
        self.assertNotEqual(dataset['dataset_id'], values[0]['dataset_id'])

    def test_incomplete_duplicate_binding_and_empty_selection_refused(self):
        values = review_fixture()
        for edit in (lambda r: r['cases'].clear(), lambda r: r['cases'][0].update(decision='unset'),
                     lambda r: r['cases'][0].update(accepted_reference_ids=[]),
                     lambda r: r['cases'][0].update(accepted_reference_ids=['invented']),
                     lambda r: r.update(audit_report_sha256='0' * 64)):
            changed = copy.deepcopy(values[-1]); edit(changed)
            with self.assertRaises(ValueError): finalise(*values[:-1], changed)

    def test_exact_original_context_and_no_all_excluded_export(self):
        values = review_fixture(); changed_public = copy.deepcopy(values[2]); changed_public['items'][0]['question'] += '?'
        review = copy.deepcopy(values[-1]); review['public_sha256'] = digest(changed_public)
        with self.assertRaisesRegex(ValueError, 'corpus'): finalise(values[0], values[1], changed_public, values[3], review)
        review = copy.deepcopy(values[-1]); review['cases'][0].update(decision='exclude', accepted_reference_ids=[])
        with self.assertRaisesRegex(ValueError, 'no accepted'): finalise(*values[:-1], review)

    def test_page_safe_text_no_default_human_attestation_or_network(self):
        _, _, public, report, _ = review_fixture()
        public = copy.deepcopy(public); public['items'][0]['question'] = '</script><img src=x onerror=alert(1)>'
        result = page(public, report)
        self.assertIn('&lt;/script&gt;', result)
        self.assertNotIn('<img src=x', result)
        self.assertIn('connect-src', result)
        self.assertNotIn('id="attest" checked', result)
        self.assertTrue(draft(public, report)['reviewer']['executor_answers_seen'])
        self.assertEqual(len(candidates(public['items'][0], report['cases'][0])), 2)


if __name__ == '__main__': unittest.main()
