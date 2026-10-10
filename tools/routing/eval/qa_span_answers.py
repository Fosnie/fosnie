"""Published supplied-passage answer-span exact match; no machine quality judgement."""
from collections import Counter
import hashlib
from pathlib import Path
import re
import string

from judge_review import digest, validate_dataset
from semantic_review import exact

VERSION = 'fosnie-published-span-em-v1'
PREFIX = 'Answer the question using only the supplied passage. Copy a short answer span from it. Reasoning is optional. End with a separate line: Final answer: <answer span>.\n\nPassage:\n'


def prompt(context, question): return PREFIX + context + '\n\nQuestion: ' + question


def fingerprint():
    return digest({'version': VERSION, 'source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})


def normalize(value):
    # Frozen lexical equivalence, not a semantic entailment or token-overlap score.
    value = ''.join(c for c in value.lower() if c not in string.punctuation)
    value = re.sub(r'\b(a|an|the)\b', ' ', value)
    return ' '.join(value.split())


def validate(dataset, bank):
    validate_dataset(dataset)
    exact(bank, ('schema_version', 'dataset_sha256', 'evaluator_sha256', 'cases'), 'invalid span bank')
    if type(bank['schema_version']) is not int or bank['schema_version'] != 1 or bank['dataset_sha256'] != digest(dataset) or bank['evaluator_sha256'] != fingerprint():
        raise ValueError('span dataset/evaluator mismatch')
    if not isinstance(bank['cases'], list) or len(bank['cases']) != len(dataset['cases']): raise ValueError('span reference coverage')
    cases = {c['id']: c for c in dataset['cases']}; seen = set()
    for row in bank['cases']:
        exact(row, ('case_id', 'article', 'paragraph_index', 'question', 'context', 'answers', 'context_sha256'), 'invalid span reference fields')
        identity = row['case_id']; context = row['context']; question = row['question']
        if (identity not in cases or identity in seen or not isinstance(row['article'], str) or not row['article'].strip()
                or type(row['paragraph_index']) is not int or row['paragraph_index'] < 0
                or not isinstance(context, str) or not 1 <= len(context) <= 2200
                or not isinstance(question, str) or not 1 <= len(question) <= 500
                or cases[identity]['conversation'] != prompt(context, question)
                or cases[identity]['family_id'] != 'squad/' + digest(' '.join(row['article'].casefold().split()))
                or row['context_sha256'] != hashlib.sha256(context.encode('utf-8')).hexdigest()):
            raise ValueError('span question/context/group binding')
        seen.add(identity)
        if not isinstance(row['answers'], list) or not 1 <= len(row['answers']) <= 16: raise ValueError('span annotation count')
        for answer in row['answers']:
            exact(answer, ('text', 'answer_start'), 'invalid span annotation')
            text, start = answer['text'], answer['answer_start']
            if (not isinstance(text, str) or not 1 <= len(text) <= 128 or not normalize(text)
                    or type(start) is not int or start < 0 or context[start:start + len(text)] != text):
                raise ValueError('published annotation is not an exact context span')
    return bank


def terminal(content):
    lines = [l.strip() for l in content.strip().splitlines() if l.strip()]; fenced = False
    if len(lines) >= 3 and lines[-1] == '```' and any(re.fullmatch(r'```[A-Za-z0-9_-]*', l) for l in lines[:-1]):
        lines.pop(); fenced = True
    markers = [i for i, l in enumerate(lines) if re.match(r'^(?:\*\*)?Final answer:', l, re.I)]
    if len(markers) > 1: return None, 'multiple_final_markers', False
    if markers and markers[0] != len(lines) - 1: return None, 'nonterminal_final_marker', False
    if not lines: return None, 'missing_value', False
    last = lines[-1]; value = None
    # Distinguish a bold label from a bold entire line or bold value.
    for expression in (r'^\*\*Final answer:\*\*\s*(.+)$', r'^\*\*Final answer:\s*(.+)\*\*$', r'^Final answer:\s*(.+)$'):
        matched = re.fullmatch(expression, last, re.I)
        if matched: value = matched[1].strip(); break
    form = 'final_marker' if value is not None else 'bare_terminal'
    if value is None:
        header = re.fullmatch(r'(?:Answer:|The (?:correct |final )?answer is\s*:?)\s*(.+)', last, re.I)
        value = header[1].strip() if header else last
        if header: form = 'terminal_header'
    for left, right in (('**', '**'), ('`', '`'), ('"', '"'), ("'", "'")):
        if len(value) > len(left) + len(right) and value.startswith(left) and value.endswith(right): value = value[len(left):-len(right)].strip()
    if not value or len(value) > 256 or not normalize(value): return None, 'unsupported_span_value', False
    return value, form, form == 'final_marker' and not fenced


def assess(reference, content, *, contract_status='completed', finish_reason='stop', target_matches=True):
    base = {'evaluator_version': VERSION, 'evaluator_sha256': fingerprint(), 'verdict': 'unknown',
            'published_span_match_evaluated': False, 'format_marker_compliant': False, 'semantic_reasoning_evaluated': False}
    if contract_status != 'completed' or finish_reason != 'stop' or target_matches is not True:
        return {**base, 'assessment_status': 'core_incomplete_or_unattested'}
    if not isinstance(content, str) or not content.strip() or len(content.encode('utf-8')) > 65536:
        return {**base, 'assessment_status': 'empty_or_oversized_answer'}
    value, form, compliant = terminal(content)
    result = {**base, 'answer_sha256': hashlib.sha256(content.encode('utf-8')).hexdigest(),
              'format_status': form, 'format_marker_compliant': compliant, 'parsed_final_value': value}
    if value is None: return {**result, 'assessment_status': 'final_value_unscorable'}
    expected = {normalize(a['text']) for a in reference['answers']}
    return {**result, 'assessment_status': 'scored_published_span', 'published_span_match_evaluated': True,
            'verdict': 'pass' if normalize(value) in expected else 'fail'}


def summarise(dataset, bank, records, strategies, baseline='gemini-lite'):
    validate(dataset, bank); cases = {c['id']: c for c in dataset['cases']}; indexed = {}
    if not strategies or len(set(strategies)) != len(strategies) or baseline not in strategies: raise ValueError('span strategy/baseline')
    for row in records:
        key = (row['strategy'], row['case_id'])
        if key[0] not in strategies or key[1] not in cases or key in indexed or row['verdict'] not in ('pass', 'fail', 'unknown'):
            raise ValueError('duplicate/foreign span result')
        indexed[key] = row
    groups, paired = [], []
    for split in ('calibration', 'test'):
        cohort = [c for c in cases.values() if c['split'] == split]
        if not cohort: continue
        for strategy in strategies:
            rows = [indexed.get((strategy, c['id']), {}) for c in cohort]; counts = Counter(r.get('verdict', 'unknown') for r in rows)
            groups.append({'strategy': strategy, 'split': split, 'cases': len(cohort), **{o: counts[o] for o in ('pass', 'fail', 'unknown')},
                'candidate_document_groups': len({c['family_id'] for c in cohort}),
                'format_marker_compliant': sum(r.get('format_marker_compliant') is True for r in rows),
                'unknown_reasons': dict(Counter(r.get('assessment_status', 'not_attempted') for r in rows if r.get('verdict', 'unknown') == 'unknown'))})
            if strategy == baseline: continue
            cells = Counter((indexed.get((baseline, c['id']), {}).get('verdict', 'unknown'), indexed.get((strategy, c['id']), {}).get('verdict', 'unknown')) for c in cohort)
            paired.append({'strategy': strategy, 'baseline': baseline, 'split': split, 'cases': len(cohort), 'cells': [
                {'baseline': a, 'candidate': b, 'count': cells[(a, b)]} for a in ('pass', 'fail', 'unknown') for b in ('pass', 'fail', 'unknown')]})
    return {'schema_version': 1, 'metric': 'normalised_published_answer_span_exact_match', 'groups': groups, 'paired': paired,
        'baseline_selected_on_current_test': False, 'baseline_response_regenerated': False, 'retrieval_exercised': False,
        'family_independence_reviewed': False, 'tier_labels_reviewed': False, 'upstream_split': 'train', 'model_training_exposure_unknown': True,
        'quality_evidence': False, 'p12_eligible': False, 'policy_updated': False, 'actual_savings_microusd': None}
