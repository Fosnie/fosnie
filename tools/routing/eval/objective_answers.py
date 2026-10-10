"""Bounded final-value accuracy for formally verified public tasks, not P12 admission."""
from collections import Counter
from decimal import Decimal, InvalidOperation
import hashlib
from pathlib import Path
import re

from judge_review import digest, validate_dataset
from semantic_review import exact
import reference_audit
from public_benchmark import INSTRUCTION

VERSION = 'fosnie-formal-final-value-v1'
DIRECTIVE = ('\n\nYou may explain your reasoning. End your answer with a separate last line: '
             'Final answer: <your final value>. Use only the value after this marker.')
TASKS = ('boolean_expressions', 'multistep_arithmetic_two', 'word_sorting', 'dyck_languages', 'navigate')


def fingerprint():
    return digest({'version': VERSION, 'module_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                   'reference_solver_sha256': hashlib.sha256(Path(reference_audit.__file__).read_bytes()).hexdigest()})


def validate(dataset, bank):
    validate_dataset(dataset)
    exact(bank, ('schema_version', 'dataset_sha256', 'evaluator_sha256', 'cases'), 'invalid formal answer bank')
    if type(bank['schema_version']) is not int or bank['schema_version'] != 1 or bank['dataset_sha256'] != digest(dataset) or bank['evaluator_sha256'] != fingerprint():
        raise ValueError('formal evaluator/dataset binding mismatch')
    if not isinstance(bank['cases'], list) or len(bank['cases']) != len(dataset['cases']):
        raise ValueError('formal reference coverage mismatch')
    cases = {c['id']: c for c in dataset['cases']}; found = set()
    for row in bank['cases']:
        exact(row, ('case_id', 'source_task', 'source_index', 'original_question', 'source_target', 'target', 'kind', 'proof',
                    'original_input_sha256'), 'invalid formal reference fields')
        identity = row['case_id']
        if identity not in cases or identity in found or row['source_task'] not in TASKS or type(row['source_index']) is not int or not 0 <= row['source_index'] < 10000:
            raise ValueError('invalid or duplicate formal reference')
        found.add(identity)
        question = row['original_question']
        if (not isinstance(question, str) or cases[identity]['conversation'] != question + DIRECTIVE
                or cases[identity]['family_id'] != 'bbh/' + row['source_task']
                or identity != f"bbh/{row['source_task']}/{row['source_index']}"
                or row['original_input_sha256'] != hashlib.sha256(question.encode('utf-8')).hexdigest()):
            raise ValueError('original source/prompt mismatch')
        solved, value, kind, proof = reference_audit.solve({'family_id': cases[identity]['family_id'], 'conversation': question + INSTRUCTION})
        if (row['source_target'] != solved or row['target'] != value or row['kind'] != kind or row['proof'] != proof):
            raise ValueError('formal computation differs from published target')
    return bank


def _unwrap(value):
    value = value.strip()
    # Only outer presentation wrappers. Never evaluate model-supplied expressions.
    for left, right in (('**', '**'), ('`', '`'), ('"', '"'), ("'", "'")):
        if len(value) > len(left) + len(right) and value.startswith(left) and value.endswith(right):
            value = value[len(left):-len(right)].strip()
    boxed = re.fullmatch(r'\\boxed\{([^{}]{1,4096})\}', value)
    return boxed.group(1).strip() if boxed else value


def canonical(kind, value):
    value = _unwrap(value)
    if kind == 'integer':
        if len(value) > 80 or not re.fullmatch(r'[+-]?(?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d*)?', value):
            return None
        try:
            number = Decimal(value.replace(',', ''))
            return str(int(number)) if number.is_finite() and number == number.to_integral_value() and abs(number) <= 10**12 else None
        except (InvalidOperation, ValueError): return None
    if kind in ('boolean', 'yes_no'):
        selected = value.removesuffix('.').casefold()
        options = {'true': 'True', 'false': 'False'} if kind == 'boolean' else {'yes': 'Yes', 'no': 'No'}
        return options.get(selected)
    if kind == 'words':
        if len(value) > 8192 or not re.fullmatch(r'[a-zA-Z]+(?:[ ,\t]+[a-zA-Z]+)*', value): return None
        words = re.split(r'[ ,\t]+', value)
        return ' '.join(words) if len(words) <= 200 else None
    if kind == 'brackets':
        selected = ''.join(value.split())
        return ' '.join(selected) if 1 <= len(selected) <= 4096 and all(c in ')]}>' for c in selected) else None
    raise ValueError('unsupported formal answer kind')


def terminal(content, kind):
    lines = [line.strip() for line in content.strip().splitlines() if line.strip()]
    fenced = False
    if len(lines) >= 3 and lines[-1] == '```' and any(re.fullmatch(r'```[A-Za-z0-9_-]*', l) for l in lines[:-1]):
        lines.pop(); fenced = True
    if not lines: return None, 'missing_final_value', False
    marker_lines = [i for i, line in enumerate(lines) if re.match(r'^(?:\*\*)?Final answer:', line, flags=re.I)]
    if len(marker_lines) > 1: return None, 'multiple_final_markers', False
    if marker_lines and marker_lines[0] != len(lines) - 1: return None, 'nonterminal_final_marker', False
    last = lines[-1]
    marker = re.fullmatch(r'(?:\*\*)?Final answer:\s*(.+)', last, flags=re.I)
    if marker:
        value = marker.group(1)
        if last.startswith('**') and value.endswith('**'): value = value[:-2]
        return canonical(kind, value), 'final_marker', not fenced
    header = re.fullmatch(r'(?:Answer:|Result:|The (?:final |correct )?answer is\s*:?)\s*(.+)', last, flags=re.I)
    return canonical(kind, header.group(1) if header else last), 'terminal_header' if header else 'bare_terminal', False


def assess(reference, content, *, contract_status='completed', finish_reason='stop', target_matches=True):
    base = {'evaluator_version': VERSION, 'evaluator_sha256': fingerprint(), 'verdict': 'unknown',
            'final_value_accuracy_evaluated': False, 'format_marker_compliant': False, 'semantic_reasoning_evaluated': False}
    if contract_status != 'completed' or finish_reason != 'stop' or target_matches is not True:
        return {**base, 'assessment_status': 'core_incomplete_or_unattested'}
    if not isinstance(content, str) or not content.strip() or len(content.encode('utf-8')) > 65536:
        return {**base, 'assessment_status': 'empty_or_oversized_answer'}
    value, form, compliant = terminal(content, reference['kind'])
    result = {**base, 'answer_sha256': hashlib.sha256(content.encode('utf-8')).hexdigest(),
              'format_status': form, 'format_marker_compliant': compliant, 'parsed_final_value': value}
    if value is None: return {**result, 'assessment_status': 'final_value_unscorable'}
    expected = canonical(reference['kind'], reference['target'])
    if expected is None: raise ValueError('invalid formal reference value')
    return {**result, 'final_value_accuracy_evaluated': True, 'assessment_status': 'scored_final_value',
            'verdict': 'pass' if value == expected else 'fail'}


def summarise(dataset, bank, records, strategies, baseline='gemini-lite'):
    validate(dataset, bank)
    if not strategies or len(set(strategies)) != len(strategies) or baseline not in strategies:
        raise ValueError('invalid frozen formal strategy/baseline')
    cases = {c['id']: c for c in dataset['cases']}; indexed = {}
    for row in records:
        key = (row['strategy'], row['case_id'])
        if key[0] not in strategies or key[1] not in cases or key in indexed or row['verdict'] not in ('pass', 'fail', 'unknown'):
            raise ValueError('duplicate/foreign formal result')
        indexed[key] = row
    groups, paired = [], []
    for split in ('calibration', 'test'):
        cohort = [c for c in cases.values() if c['split'] == split]
        if not cohort: continue
        for strategy in strategies:
            rows = [indexed.get((strategy, c['id']), {}) for c in cohort]
            counts = Counter(row.get('verdict', 'unknown') for row in rows)
            groups.append({'strategy': strategy, 'split': split, 'cases': len(cohort),
                **{o: counts[o] for o in ('pass', 'fail', 'unknown')},
                'format_marker_compliant': sum(r.get('format_marker_compliant') is True for r in rows),
                'unknown_reasons': dict(Counter(r.get('assessment_status', 'not_attempted') for r in rows if r.get('verdict', 'unknown') == 'unknown')),
                'candidate_task_groups': len({c['family_id'] for c in cohort})})
            if strategy == baseline: continue
            cells = Counter((indexed.get((baseline, c['id']), {}).get('verdict', 'unknown'),
                             indexed.get((strategy, c['id']), {}).get('verdict', 'unknown')) for c in cohort)
            paired.append({'strategy': strategy, 'baseline': baseline, 'split': split, 'cases': len(cohort),
                'cells': [{'baseline': a, 'candidate': b, 'count': cells[(a, b)]} for a in ('pass', 'fail', 'unknown') for b in ('pass', 'fail', 'unknown')]})
    return {'schema_version': 1, 'metric': 'formally_verified_final_value_accuracy', 'groups': groups, 'paired': paired,
        'baseline_selected_on_current_test': False, 'baseline_response_regenerated': False, 'full_reasoning_correctness_evaluated': False,
        'family_independence_reviewed': False, 'workload_tier_labels_reviewed': False, 'public_training_exposure_unknown': True,
        'quality_evidence': False, 'p12_eligible': False, 'policy_updated': False, 'actual_savings_microusd': None}
