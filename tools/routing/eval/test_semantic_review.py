import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from judge_review import digest
from semantic_review import (bind_run_rubric, finalise, inputs, prepare, prepare_files,
                             propose_rubric, read, validate_bundle, validate_rubric)


class SemanticReviewTests(unittest.TestCase):
    def fixture(self):
        dataset = {'schema_version': 1, 'dataset_id': 'synthetic-semantic',
                   'source': {'kind': 'synthetic', 'reference': 'contract fixture'},
                   'cases': [{'id': str(i), 'family_id': 'family-' + str(i),
                              'split': 'calibration' if i == 0 else 'test', 'language': 'en',
                              'conversation': 'Question ' + str(i)} for i in range(3)]}
        rubric = propose_rubric(dataset)
        allocation = {'dataset_sha256': digest(dataset), 'evaluator_sha256': 'a' * 64,
                      'strategies': ['baseline', 'auto']}
        answers, rows = [], []
        for case in dataset['cases']:
            for strategy in allocation['strategies']:
                evaluation = strategy + '-' + case['id']
                response = 'Answer for ' + case['id']
                sha = hashlib.sha256(response.encode()).hexdigest()
                rows.append({'case_id': case['id'], 'strategy': strategy, 'evaluation_id': evaluation,
                             'answer_sha256': sha, 'source': 'synthetic', 'contract_status': 'completed',
                             'finish_reason': 'stop', 'target_matches': True,
                             'actual': {'model': 'NEVER_IN_PAGE'}, 'profile_sha256': 'b' * 64})
                answers.append({'case_id': case['id'], 'strategy': strategy, 'evaluation_id': evaluation,
                                'response': response, 'answer_sha256': sha, 'evaluator_sha256': 'a' * 64})
        report = {'schema_version': 1, 'source': 'synthetic', 'dataset_sha256': digest(dataset),
                  'allocation_sha256': digest(allocation), 'evaluator_sha256': 'a' * 64, 'records': rows}
        return dataset, rubric, report, allocation, answers

    def bundle(self):
        return prepare(*self.fixture(), 'baseline', 'retrospective')

    def complete(self, bundle, kind='machine'):
        review = copy.deepcopy(bundle[3])
        review['reviewer'] = {'kind': kind, 'id': 'synthetic-' + kind, 'blinded_to_metadata': kind == 'human'}
        review['complete'] = True
        for row in review['items']:
            for rating in row['ratings']:
                if rating['outcome'] is None:
                    rating.update(outcome='pass', rationale='Synthetic fixture is complete.')
        return review

    def test_machine_reviews_remain_diagnostic_and_splits_are_separate(self):
        bundle = self.bundle()
        result = finalise(*bundle[:3], self.complete(bundle))
        self.assertEqual(result['review_status'], 'machine_diagnostic')
        self.assertFalse(result['quality_evidence']); self.assertFalse(result['p12_eligible'])
        self.assertEqual([(r['split'], r['cases']) for r in result['groups']],
                         [('calibration', 1), ('calibration', 1), ('test', 2), ('test', 2)])
        self.assertEqual(result['paired'][1]['cases'], 2)

    def test_no_answer_regeneration_and_metadata_not_in_public_bundle(self):
        public, private, manifest, _ = self.bundle()
        self.assertNotIn('NEVER_IN_PAGE', json.dumps(public))
        self.assertNotIn('profile_sha256', json.dumps(public))
        self.assertEqual(len(public['items']), 6)
        self.assertEqual(len({row['blind_id'] for row in public['items']}), 6)
        validate_bundle(public, private, manifest)

    def test_missing_and_truncated_and_wrong_target_answers_keep_denominator(self):
        dataset, rubric, report, allocation, answers = self.fixture()
        report['records'] = report['records'][1:]
        answers = answers[1:]
        report['records'][0]['finish_reason'] = 'length'
        report['records'][1]['target_matches'] = False
        answers.pop(2)
        bundle = prepare(dataset, rubric, report, allocation, answers, 'baseline', 'retrospective')
        unavailable = [item for item in bundle[0]['items'] if not item['available']]
        self.assertEqual(len(unavailable), 4)
        result = finalise(*bundle[:3], self.complete(bundle))
        self.assertEqual(sum(r['unknown'] for r in result['groups']), 4)
        review = self.complete(bundle)
        blind_id = unavailable[0]['blind_id']
        next(r for r in review['items'] if r['blind_id'] == blind_id)['ratings'][0]['outcome'] = 'pass'
        with self.assertRaisesRegex(ValueError, 'unavailable'): finalise(*bundle[:3], review)

    def test_fail_not_hidden_by_unknown_and_optional_defects_do_not_fail(self):
        bundle = self.bundle(); review = self.complete(bundle)
        review['items'][0]['ratings'][0]['outcome'] = 'fail'
        review['items'][0]['ratings'][1]['outcome'] = 'unknown'
        result = finalise(*bundle[:3], review)
        self.assertEqual(sum(row['fail'] for row in result['groups']), 1)
        data = self.fixture(); data[1]['cases'][0]['criteria'][-1]['required'] = False
        bundle = prepare(*data, 'baseline', 'retrospective'); review = self.complete(bundle)
        review['items'][0]['ratings'][-1]['outcome'] = 'fail'
        self.assertEqual(sum(row['fail'] for row in finalise(*bundle[:3], review)['groups']), 0)

    def test_human_attestation_is_explicit_and_still_no_admission(self):
        bundle = self.bundle(); review = self.complete(bundle, 'human')
        review['reviewer']['blinded_to_metadata'] = False
        with self.assertRaisesRegex(ValueError, 'blinding'): finalise(*bundle[:3], review)
        review['reviewer']['blinded_to_metadata'] = True
        result = finalise(*bundle[:3], review)
        self.assertEqual(result['review_status'], 'human_attested')
        self.assertFalse(result['p12_eligible'])

    def test_foreign_duplicate_and_missing_reviews_rejected(self):
        bundle = self.bundle()
        for mutate in (lambda r: r['items'].pop(), lambda r: r['items'].append(r['items'][0]),
                       lambda r: r['items'][0].update(blind_id='0' * 32),
                       lambda r: r['items'][0]['ratings'].pop(),
                       lambda r: r['items'][0]['ratings'][0].update(criterion_id='foreign'),
                       lambda r: r.update(complete=False)):
            review = self.complete(bundle); mutate(review)
            with self.assertRaises(ValueError): finalise(*bundle[:3], review)

    def test_answer_and_evaluation_binding_rejected(self):
        for mutate in (lambda a: a.update(response='changed'), lambda a: a.update(evaluation_id='other'),
                       lambda a: a.update(evaluator_sha256='0' * 64)):
            data = self.fixture(); mutate(data[4][0])
            with self.assertRaisesRegex(ValueError, 'attestation'): inputs(*data, 'baseline')
        data = self.fixture(); data[4].append(data[4][0])
        with self.assertRaises(ValueError): inputs(*data, 'baseline')
        data = self.fixture(); data[2]['records'].append(data[2]['records'][0])
        with self.assertRaises(ValueError): inputs(*data, 'baseline')

    def test_manifest_tampering_and_rehashed_public_key_mismatch_rejected(self):
        public, private, manifest, _ = self.bundle()
        public['items'][0]['response'] = 'tampered'
        with self.assertRaisesRegex(ValueError, 'changed'): validate_bundle(public, private, manifest)
        manifest['public_sha256'] = digest(public)
        with self.assertRaisesRegex(ValueError, 'mismatch'): validate_bundle(public, private, manifest)

    def test_rubric_reference_snapshot_and_criterion_bounds(self):
        dataset, rubric, *_ = self.fixture()
        rubric['cases'][0]['references'] = [{'reference': 'provided-text', 'snapshot_sha256': '0' * 64, 'excerpt': 'Fact snapshot'}]
        with self.assertRaisesRegex(ValueError, 'reference'): validate_rubric(dataset, rubric)
        rubric['cases'][0]['references'][0]['snapshot_sha256'] = hashlib.sha256(b'Fact snapshot').hexdigest()
        validate_rubric(dataset, rubric)
        rubric['cases'][0]['criteria'][1]['id'] = rubric['cases'][0]['criteria'][0]['id']
        with self.assertRaisesRegex(ValueError, 'duplicate'): validate_rubric(dataset, rubric)

    def test_predeclared_requires_run_binding_and_unseen_answers(self):
        data = self.fixture()
        with self.assertRaisesRegex(ValueError, 'before inference'): prepare(*data, 'baseline', 'predeclared')
        data[3]['semantic_rubric_sha256'] = digest(data[1])
        data[2].update(allocation_sha256=digest(data[3]), semantic_rubric_sha256=digest(data[1]))
        bundle = prepare(*data, 'baseline', 'predeclared')
        validate_bundle(*bundle[:3])
        data[1]['author']['answers_seen'] = True
        data[3]['semantic_rubric_sha256'] = digest(data[1])
        with self.assertRaisesRegex(ValueError, 'already seen'): bind_run_rubric(data[0], data[3], data[1])

    def test_legacy_runs_unchanged_and_missing_rubric_refused(self):
        dataset, rubric, _, allocation, _ = self.fixture()
        self.assertIsNone(bind_run_rubric(dataset, allocation, None))
        with self.assertRaises(ValueError): bind_run_rubric(dataset, allocation, rubric)
        allocation['semantic_rubric_sha256'] = digest(rubric)
        with self.assertRaises(ValueError): bind_run_rubric(dataset, allocation, None)
        self.assertEqual(bind_run_rubric(dataset, allocation, rubric), digest(rubric))

    def test_execute_refuses_changed_or_missing_rubric_before_dispatch_or_files(self):
        from instruction_following import evaluator_fingerprint
        from instruction_pilot import execute
        dataset, *_ = self.fixture()
        dataset['cases'] = [{'id': str(i), 'family_id': str(i), 'split': 'test', 'language': 'en',
                             'conversation': 'Question ' + str(i)} for i in range(40)]
        rubric = propose_rubric(dataset)
        plan = {'dataset_sha256': digest(dataset), 'evaluator_sha256': evaluator_fingerprint(),
                'maximum_generation_calls': 280, 'maximum_primary_calls': 200,
                'semantic_rubric_sha256': digest(rubric)}
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'unused'
            for supplied in (None, {**rubric, 'dataset_sha256': '0' * 64}):
                with self.assertRaises(ValueError):
                    execute(dataset, {}, {}, plan, output, None, None, semantic_rubric=supplied)
                self.assertFalse(output.exists())

    def test_new_rubric_binding_cannot_be_retroactively_forged_in_public_mode(self):
        public, private, manifest, _ = self.bundle()
        public['mode'] = 'predeclared'; manifest['public_sha256'] = digest(public)
        with self.assertRaisesRegex(ValueError, 'frozen|binding'): validate_bundle(public, private, manifest)

    def test_exclusive_output_and_inert_html_payload(self):
        data = self.fixture()
        data[0]['cases'][0]['conversation'] = '</script><script>window.__sourceExecuted=true</script>'
        data[1]['dataset_sha256'] = digest(data[0]); data[3]['dataset_sha256'] = digest(data[0])
        data[2].update(dataset_sha256=digest(data[0]), allocation_sha256=digest(data[3]))
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'review'
            prepare_files(output, *data, 'baseline', 'retrospective')
            page = (output / 'review.html').read_text(encoding='utf-8')
            self.assertNotIn('</script><script>window', page)
            self.assertNotIn('NEVER_IN_PAGE', page)
            self.assertIn('\\u003c/script\\u003e', page)
            with self.assertRaises(FileExistsError): prepare_files(output, *data, 'baseline', 'retrospective')

    def test_json_duplicate_nonfinite_and_size_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'input.json'
            for raw in ('{"a":1,"a":2}', '{"x":NaN}'):
                path.write_text(raw, encoding='utf-8')
                with self.assertRaises(ValueError): read(path)
            path.write_bytes(b' ' * 65)
            with patch('semantic_review.LIMIT', 64), self.assertRaises(ValueError): read(path)


if __name__ == '__main__':
    unittest.main()
