from copy import deepcopy
import unittest
from unittest.mock import patch
import instruction_following as evaluator

CASES = [
    ('keywords:existence', {'keywords': ['alpha']}, 'ALPHA beta', 'beta'),
    ('keywords:frequency', {'keyword': 'alpha', 'frequency': 2, 'relation': 'at least'}, 'alpha ALPHA', 'alpha'),
    ('keywords:forbidden_words', {'forbidden_words': ['alpha']}, 'alphabet', 'alpha'),
    ('keywords:letter_frequency', {'letter': 'a', 'let_frequency': 2, 'let_relation': 'at least'}, 'AA', 'b'),
    ('length_constraints:number_words', {'num_words': 2, 'relation': 'at least'}, 'one,two', 'one'),
    ('length_constraints:number_paragraphs', {'num_paragraphs': 2}, 'one***two', 'one\n\ntwo'),
    ('length_constraints:nth_paragraph_first_word', {'num_paragraphs': 2, 'nth_paragraph': 2, 'first_word': 'hello'}, 'one\n\nHello, world', 'one\n\nother world'),
    ('detectable_content:number_placeholders', {'num_placeholders': 2}, '[one] [two]', '[one]'),
    ('detectable_content:postscript', {'postscript_marker': 'P.S.'}, 'Main\nP.S. extra', 'Main'),
    ('detectable_format:number_bullet_lists', {'num_bullets': 2}, '* one\n- two', '* one'),
    ('detectable_format:constrained_response', {}, evaluator.checks.CONSTRAINED_RESPONSES[0], 'unlisted'),
    ('detectable_format:number_highlighted_sections', {'num_highlights': 2}, '*one* **two**', '*one*'),
    ('detectable_format:multiple_sections', {'section_spliter': 'Section', 'num_sections': 2}, 'Section 1 one\nSection 2 two', 'Section 1 one'),
    ('detectable_format:json_format', {}, '```json\n{"a":1}\n```', 'not json'),
    ('detectable_format:title', {}, '<<Title>>\ntext', 'Title\ntext'),
    ('combination:two_responses', {}, 'first******second', 'first******first'),
    ('combination:repeat_prompt', {'prompt_to_repeat': 'Repeat me'}, 'Repeat me then answer', 'answer'),
    ('startend:end_checker', {'end_phrase': 'The end.'}, 'Text The end.', 'The end. Extra'),
    ('startend:quotation', {}, '"text"', 'text'),
    ('punctuation:no_comma', {}, 'one two', 'one,two'),
]

def constraints(identity, kwargs): return {'instruction_ids': [identity], 'kwargs': [kwargs]}
def assess(value, text, **overrides):
    return evaluator.assess(value, text, **({'contract_status': 'completed', 'finish_reason': 'stop', 'target_matches': True} | overrides))

class InstructionFollowingTests(unittest.TestCase):
    def test_response_bound_and_combined_checks(self):
        value = {'instruction_ids': ['punctuation:no_comma', 'startend:quotation'], 'kwargs': [{}, {}]}
        self.assertEqual(assess(value, '"one two"')['verdict'], 'pass')
        self.assertEqual(assess(value, '"one,two"')['verdict'], 'fail')
        self.assertEqual(assess(value, 'x' * 65537)['verdict'], 'unknown')

    def test_recomputed_score_missing_denominator_and_duplicate_refusal(self):
        from judge_review import digest
        dataset = {'schema_version': 1, 'dataset_id': 'test-instructions', 'source': {'kind': 'synthetic', 'reference': 'unit fixture'},
                   'cases': [{'id': str(i), 'family_id': 'family-' + str(i), 'split': 'test', 'language': 'en', 'conversation': 'Prompt ' + str(i)} for i in range(2)]}
        config = {'id': 'model', 'model': 'fixture', 'effort': 'none', 'profile_sha256': 'a' * 64}
        observations = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'evaluator_version': evaluator.VERSION,
                        'evaluator_sha256': evaluator.evaluator_fingerprint(), 'configurations': [config], 'records': [
                            {'configuration_id': 'model', 'case_id': '0', 'response': 'bad,answer', 'verdict': 'pass',
                             'effort': 'none', 'profile_sha256': 'a' * 64, 'contract_status': 'completed', 'finish_reason': 'stop', 'target_matches': True}]}
        with patch.object(evaluator, 'corpus', return_value=(dataset, {str(i): constraints('punctuation:no_comma', {}) for i in range(2)}, {})):
            report = evaluator.comparison('unused', observations)
            test = next(row for row in report['groups'] if row['split'] == 'test')
            self.assertEqual((test['pass'], test['fail'], test['unknown'], test['families']), (0, 1, 1, 2))
            self.assertFalse(report['p12_eligible'])
            self.assertFalse(report['semantic_quality_evaluated'])
            self.assertTrue(all('response' not in row for row in report['instruction_results']))
            observations['records'].append(deepcopy(observations['records'][0]))
            with self.assertRaisesRegex(ValueError, 'duplicate'):
                evaluator.comparison('unused', observations)


class InstructionCorpusBindingTests(unittest.TestCase):
    def fixture(self, directory):
        import hashlib, json
        from pathlib import Path
        from judge_review import digest, write_new
        raw = (json.dumps({'key': 1, 'prompt': 'Include alpha.', 'instruction_id_list': ['keywords:existence'], 'kwargs': [{'keywords': ['alpha']}]}) + '\n').encode()
        dataset = {'schema_version': 1, 'dataset_id': 'binding-fixture', 'source': {'kind': 'synthetic', 'reference': 'unit only'},
                   'cases': [{'id': 'ifeval/1', 'family_id': 'ifeval/keywords:existence', 'split': 'test', 'language': 'en', 'conversation': 'Include alpha.'}]}
        bank = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'cases': [{'case_id': 'ifeval/1', 'constraints': constraints('keywords:existence', {'keywords': ['alpha']})}]}
        sha = hashlib.sha256(raw).hexdigest()
        manifest = {'dataset_sha256': digest(dataset), 'constraints_sha256': digest(bank), 'evaluator_version': evaluator.VERSION,
                    'evaluator_sha256': evaluator.evaluator_fingerprint(), 'source_revision': evaluator.SOURCE_REVISION, 'source_data_sha256': sha}
        folder = Path(directory)
        for name, value in (('dataset', dataset), ('constraints', bank), ('manifest', manifest)): write_new(folder / (name + '.json'), value)
        (folder / 'upstream-input_data.jsonl').write_bytes(raw)
        return folder, sha, dataset, bank, manifest

    def test_rehashed_prompt_or_kwargs_still_refused_against_published_source(self):
        import json, tempfile
        from judge_review import digest
        for change in ('prompt', 'kwargs'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                folder, sha, dataset, bank, manifest = self.fixture(directory)
                with patch.object(evaluator, 'SOURCE_SHA256', sha):
                    evaluator.corpus(folder)
                    if change == 'prompt': dataset['cases'][0]['conversation'] = 'Changed prompt.'
                    else: bank['cases'][0]['constraints']['kwargs'][0]['keywords'] = ['beta']
                    bank['dataset_sha256'] = digest(dataset)
                    manifest['dataset_sha256'] = digest(dataset)
                    manifest['constraints_sha256'] = digest(bank)
                    for name, value in (('dataset', dataset), ('constraints', bank), ('manifest', manifest)):
                        (folder / (name + '.json')).write_text(json.dumps(value), encoding='utf-8')
                    with self.assertRaisesRegex(ValueError, 'published|upstream'):
                        evaluator.corpus(folder)

    def test_changed_raw_source_or_evaluator_cannot_score_old_manifest(self):
        import tempfile
        for change in ('source', 'evaluator'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                folder, sha, _, _, _ = self.fixture(directory)
                with patch.object(evaluator, 'SOURCE_SHA256', sha):
                    if change == 'source':
                        (folder / 'upstream-input_data.jsonl').write_bytes(b'changed')
                        with self.assertRaisesRegex(ValueError, 'upstream'): evaluator.corpus(folder)
                    else:
                        with patch.object(evaluator, 'evaluator_fingerprint', return_value='0' * 64):
                            with self.assertRaisesRegex(ValueError, 'evaluator'): evaluator.corpus(folder)


def register_constraints(identity, kwargs, passing, failing):
    def test(self):
        value = constraints(identity, kwargs)
        self.assertEqual(assess(value, passing)['verdict'], 'pass')
        self.assertEqual(assess(value, failing)['verdict'], 'fail')
        self.assertEqual(assess(value, ' \n')['verdict'], 'fail')
    setattr(InstructionFollowingTests, 'test_' + identity.replace(':', '_'), test)

for case in CASES:
    register_constraints(*case)

def register_unknown(number, overrides):
    def test(self):
        self.assertEqual(assess(constraints('punctuation:no_comma', {}), 'valid', **overrides)['verdict'], 'unknown')
    setattr(InstructionFollowingTests, 'test_unknown_' + str(number), test)

for number, override in enumerate([{'contract_status': 'failed'}, {'finish_reason': 'length'}, {'target_matches': False}]):
    register_unknown(number, override)

def register_invalid(number, value):
    def test(self):
        with self.assertRaises(ValueError): evaluator.validate_constraints(value)
    setattr(InstructionFollowingTests, 'test_invalid_' + str(number), test)

for number, value in enumerate([
    constraints('language:response_language', {'language': 'en'}),
    constraints('keywords:existence', {'keywords': ['(a+)+$']}),
    constraints('keywords:frequency', {'keyword': 'x', 'frequency': None, 'relation': 'at least'}),
    constraints('keywords:frequency', {'keyword': 'x', 'frequency': True, 'relation': 'at least'}),
    constraints('punctuation:no_comma', {'extra': 'ignored'}),
    {'instruction_ids': ['punctuation:no_comma'], 'kwargs': []},
]):
    register_invalid(number, value)

if __name__ == '__main__': unittest.main()
