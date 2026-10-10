"""Bounded, versioned IFEval instruction assessment, separate from semantics."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re

from judge_review import digest, load, prompt_digest, validate_dataset, write_new
import _ifeval_pinned as checks

VERSION = 'fosnie-ifeval-strict-subset-v1'
SOURCE_REVISION = 'e6890f85757dd84e27ca6df2dd30651dafad28e0'
SOURCE_SHA256 = '67ffeee0fcb87c317c5b08a2de85557b4a7e96ada6178aa645b4954fe4b53d49'

# (class, explicit published kwargs -> checker fields). No random defaults,
# language detector, sentence/word-tokenizer models or network dependencies.
REGISTRY = {
    'keywords:existence': ('KeywordChecker', {'keywords': '_keywords'}),
    'keywords:frequency': ('KeywordFrequencyChecker', {'keyword': '_keyword', 'frequency': '_frequency', 'relation': '_comparison_relation'}),
    'keywords:forbidden_words': ('ForbiddenWords', {'forbidden_words': '_forbidden_words'}),
    'keywords:letter_frequency': ('LetterFrequencyChecker', {'letter': '_letter', 'let_frequency': '_frequency', 'let_relation': '_comparison_relation'}),
    'length_constraints:number_words': ('NumberOfWords', {'num_words': '_num_words', 'relation': '_comparison_relation'}),
    'length_constraints:number_paragraphs': ('ParagraphChecker', {'num_paragraphs': '_num_paragraphs'}),
    'length_constraints:nth_paragraph_first_word': ('ParagraphFirstWordCheck', {'num_paragraphs': '_num_paragraphs', 'nth_paragraph': '_nth_paragraph', 'first_word': '_first_word'}),
    'detectable_content:number_placeholders': ('PlaceholderChecker', {'num_placeholders': '_num_placeholders'}),
    'detectable_content:postscript': ('PostscriptChecker', {'postscript_marker': '_postscript_marker'}),
    'detectable_format:number_bullet_lists': ('BulletListChecker', {'num_bullets': '_num_bullets'}),
    'detectable_format:constrained_response': ('ConstrainedResponseChecker', {}),
    'detectable_format:number_highlighted_sections': ('HighlightSectionChecker', {'num_highlights': '_num_highlights'}),
    'detectable_format:multiple_sections': ('SectionChecker', {'section_spliter': '_section_spliter', 'num_sections': '_num_sections'}),
    'detectable_format:json_format': ('JsonFormat', {}),
    'detectable_format:title': ('TitleChecker', {}),
    'combination:two_responses': ('TwoResponsesChecker', {}),
    'combination:repeat_prompt': ('RepeatPromptThenAnswer', {'prompt_to_repeat': '_prompt_to_repeat'}),
    'startend:end_checker': ('EndChecker', {'end_phrase': '_end_phrase'}),
    'startend:quotation': ('QuotationChecker', {}),
    'punctuation:no_comma': ('CommaChecker', {}),
}


class Unsupported(ValueError): pass


def evaluator_fingerprint():
    return digest({'version': VERSION, 'module_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                   'checks_sha256': hashlib.sha256(Path(checks.__file__).read_bytes()).hexdigest()})


def checker(identity, kwargs):
    if identity not in REGISTRY:
        raise Unsupported('instruction outside the pinned subset')
    name, fields = REGISTRY[identity]
    if not isinstance(kwargs, dict) or set(kwargs) != set(fields):
        raise Unsupported('explicit instruction argument coverage')
    for key, value in kwargs.items():
        if key in ('relation', 'let_relation'):
            if value not in ('less than', 'at least'): raise Unsupported('comparison relation')
        elif key.startswith('num_') or key in ('frequency', 'let_frequency', 'nth_paragraph'):
            if type(value) is not int or not 1 <= value <= 512: raise Unsupported('integer instruction bound')
        elif key in ('keywords', 'forbidden_words'):
            if not isinstance(value, list) or not 1 <= len(value) <= 32 or any(not safe_pattern(item) for item in value):
                raise Unsupported('literal keyword list bound')
        elif key in ('keyword', 'section_spliter', 'postscript_marker'):
            if not safe_pattern(value): raise Unsupported('bounded pattern argument')
        elif key == 'letter':
            if not isinstance(value, str) or not re.fullmatch('[a-z]', value): raise Unsupported('letter argument')
        else:
            if not isinstance(value, str) or not value.strip() or len(value.encode()) > 4096 or any(ord(c) < 32 and c not in '\n\t' for c in value):
                raise Unsupported('text instruction argument')
    if 'nth_paragraph' in kwargs and kwargs['nth_paragraph'] > kwargs['num_paragraphs']:
        raise Unsupported('paragraph index')
    result = getattr(checks, name)(identity)
    for key, field in fields.items(): setattr(result, field, kwargs[key])
    if name == 'ConstrainedResponseChecker': result._constrained_responses = checks.CONSTRAINED_RESPONSES
    return result


def safe_pattern(value):
    # Upstream treats some kwargs as regex patterns. Refuse executable/expensive
    # patterns rather than interpreting arbitrary downloaded expressions.
    return isinstance(value, str) and bool(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 _.,'-]{0,127}", value))


def validate_constraints(value):
    if not isinstance(value, dict) or set(value) != {'instruction_ids', 'kwargs'}:
        raise ValueError('constraint shape')
    identities, arguments = value['instruction_ids'], value['kwargs']
    if not isinstance(identities, list) or not 1 <= len(identities) <= 8 or not isinstance(arguments, list) or len(arguments) != len(identities):
        raise ValueError('constraint count')
    if len(set(identities)) != len(identities): raise ValueError('duplicate instruction')
    for identity, kwargs in zip(identities, arguments): checker(identity, kwargs)
    return value


def assess(constraints, response, *, contract_status, finish_reason, target_matches):
    validate_constraints(constraints)
    base = {'evaluator_version': VERSION, 'constraint_sha256': digest(constraints),
            'semantic_quality_evaluated': False, 'verdict': 'unknown', 'instruction_results': []}
    if contract_status != 'completed' or finish_reason != 'stop' or target_matches is not True:
        return {**base, 'reason': 'incomplete_or_unattested_response'}
    if not isinstance(response, str) or len(response.encode()) > 65536:
        return {**base, 'reason': 'response_byte_bound'}
    results = [{'instruction_id': identity, 'passed': bool(response.strip()) and bool(checker(identity, kwargs).check_following(response))}
               for identity, kwargs in zip(constraints['instruction_ids'], constraints['kwargs'])]
    return {**base, 'verdict': 'pass' if all(row['passed'] for row in results) else 'fail', 'instruction_results': results}


def corpus(directory):
    directory = Path(directory)
    dataset, bank, manifest = [load(directory / (name + '.json')) for name in ('dataset', 'constraints', 'manifest')]
    validate_dataset(dataset)
    if manifest['dataset_sha256'] != digest(dataset) or manifest['constraints_sha256'] != digest(bank) or bank['dataset_sha256'] != digest(dataset):
        raise ValueError('instruction corpus binding')
    if manifest['evaluator_version'] != VERSION or manifest['source_revision'] != SOURCE_REVISION or manifest['source_data_sha256'] != SOURCE_SHA256:
        raise ValueError('instruction source/evaluator binding')
    if manifest['evaluator_sha256'] != evaluator_fingerprint(): raise ValueError('changed evaluator')
    raw = (directory / 'upstream-input_data.jsonl').read_bytes()
    if len(raw) > 2 * 1024 * 1024 or hashlib.sha256(raw).hexdigest() != SOURCE_SHA256:
        raise ValueError('changed upstream source')
    upstream = {str(row['key']): row for row in [json.loads(line) for line in raw.splitlines()]}
    indexed = {}
    for row in bank['cases']:
        if not isinstance(row, dict) or set(row) != {'case_id', 'constraints'} or row['case_id'] in indexed:
            raise ValueError('constraint case identity')
        indexed[row['case_id']] = validate_constraints(row['constraints'])
    if set(indexed) != {case['id'] for case in dataset['cases']}: raise ValueError('constraint coverage')
    for case in dataset['cases']:
        original = upstream.get(case['id'].removeprefix('ifeval/'))
        if not original or case['id'] != 'ifeval/' + str(original['key']) or case['conversation'] != original['prompt']:
            raise ValueError('case differs from published prompt')
        expected = {'instruction_ids': original['instruction_id_list'], 'kwargs': original['kwargs']}
        if indexed[case['id']] != expected or len(expected['instruction_ids']) != 1 or case['family_id'] != 'ifeval/' + expected['instruction_ids'][0]:
            raise ValueError('constraint or candidate family differs from upstream')
    return dataset, indexed, manifest


def comparison(directory, observations):
    from cohort_report import summarise
    dataset, constraints, manifest = corpus(directory)
    if observations.get('schema_version') != 1 or observations.get('dataset_sha256') != digest(dataset) or observations.get('evaluator_version') != VERSION or observations.get('evaluator_sha256') != evaluator_fingerprint():
        raise ValueError('instruction observation binding')
    records = observations.get('records')
    if not isinstance(records, list) or len(records) > 32000: raise ValueError('record bound')
    scored = []
    for row in records:
        if not isinstance(row, dict) or row.get('case_id') not in constraints: raise ValueError('foreign instruction observation')
        # Recompute from the original response; never trust supplied pass labels.
        result = assess(constraints[row['case_id']], row.get('response'), contract_status=row.get('contract_status'),
                        finish_reason=row.get('finish_reason'), target_matches=row.get('target_matches'))
        scored.append({key: value for key, value in row.items() if key != 'response'} | result)
    report = summarise(dataset, {'schema_version': 1, 'dataset_sha256': digest(dataset),
                                'configurations': observations['configurations'], 'records': scored})
    report.update(evaluator_version=VERSION, semantic_quality_evaluated=False, p12_eligible=False,
                  instruction_results=scored, source_manifest_sha256=digest(manifest),
                  scoring_scope='Pinned strict verifiable instructions only; not semantic answer accuracy',
                  readiness_reasons=['no_reviewed_independence', 'no_reviewed_workload_labels', 'no_prompt_kwargs_consistency_review'])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cohort', required=True)
    parser.add_argument('--observations')
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    if args.observations:
        result = comparison(args.cohort, load(args.observations))
    else:
        dataset, constraints, manifest = corpus(args.cohort)
        result = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'case_count': len(dataset['cases']),
                  'candidate_family_count': len({case['family_id'] for case in dataset['cases']}),
                  'evaluator_version': VERSION, 'model_calls': 0, 'quality_evidence': False, 'p12_eligible': False,
                  'source_manifest_sha256': digest(manifest), 'review_complete': False}
    write_new(args.output, result)
    print(json.dumps({key: result.get(key) for key in ('case_count', 'candidate_family_count', 'model_calls', 'p12_eligible')}))

if __name__ == '__main__': main()
