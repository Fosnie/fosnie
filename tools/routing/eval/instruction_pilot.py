"""One frozen Core run: manual responses and paired instruction-only Auto scores."""
from collections import Counter
from pathlib import Path
import time
from audited_run import append_reserved, payload, preflight, score, snapshot
from instruction_following import assess, evaluator_fingerprint
from judge_review import digest, write_new
from semantic_review import bind_run_rubric

STRATEGIES = ('qwen4b', 'openai', 'gemini-lite', 'classifier', 'hybrid')


def summarise(dataset, records, strategies=STRATEGIES):
    cases = {case['id']: case for case in dataset['cases']}
    indexed = {}
    for row in records:
        key = (row['strategy'], row['case_id'])
        if row['strategy'] not in strategies or row['case_id'] not in cases or key in indexed or row['verdict'] not in ('pass', 'fail', 'unknown'):
            raise ValueError('foreign/duplicate instruction result')
        indexed[key] = row
    groups = []
    families = {case['family_id'] for case in cases.values()}
    for strategy in strategies:
        rows = [indexed.get((strategy, case['id']), {}) for case in cases.values()]
        counts = Counter(row.get('verdict', 'unknown') for row in rows)
        groups.append({'strategy': strategy, 'cases': len(cases), **{name: counts[name] for name in ('pass', 'fail', 'unknown')},
                       'candidate_type_groups': len(families), 'fully_passed_type_groups': sum(
                           all(indexed.get((strategy, case['id']), {}).get('verdict') == 'pass'
                               for case in cases.values() if case['family_id'] == family) for family in families),
                       'completed_core_calls': sum(row.get('usage', {}).get('calls', 0) for row in rows),
                       'judge_calls': sum(row.get('classifier_ledger', {}).get('calls', 0) for row in rows)})
    paired = []
    for strategy in ('classifier', 'hybrid'):
        counts = Counter()
        for identity in cases:
            baseline = indexed.get(('gemini-lite', identity), {}).get('verdict') == 'pass'
            routed = indexed.get((strategy, identity), {}).get('verdict') == 'pass'
            counts['both_pass' if baseline and routed else 'baseline_only' if baseline else 'auto_only' if routed else 'neither_pass'] += 1
        paired.append({'strategy': strategy, 'baseline': 'gemini-lite', 'cases': len(cases), **counts})
    return {'schema_version': 1, 'groups': groups, 'paired_test': paired,
            'baseline_selected_on_test': False, 'baseline_response_regenerated': False,
            'semantic_quality_evaluated': False, 'family_independence_reviewed': False,
            'actual_savings_microusd': None, 'quality_evidence': False, 'p12_eligible': False}


def execute(dataset, constraints, config, plan, output, client, guard, clock=time.monotonic, progress=None, semantic_rubric=None):
    if plan['dataset_sha256'] != digest(dataset) or plan['evaluator_sha256'] != evaluator_fingerprint():
        raise ValueError('frozen instruction inputs changed')
    strategies=tuple(plan.get('strategies', STRATEGIES))
    expected_primary=len(dataset['cases'])*len(strategies)
    expected_total=len(dataset['cases'])*sum(2 if name in ('classifier','hybrid') else 1 for name in strategies)
    if (len(dataset['cases']) != 40 or not strategies or len(set(strategies))!=len(strategies)
            or any(name not in STRATEGIES for name in strategies)
            or not expected_total<=280 or plan['maximum_generation_calls']!=expected_total or plan['maximum_primary_calls']!=expected_primary):
        raise ValueError('whole instruction experiment shape changed')
    semantic_hash = bind_run_rubric(dataset, plan, semantic_rubric)
    output = Path(output)
    output.mkdir(exist_ok=False)
    write_new(output / 'allocation.json', plan)
    if semantic_hash is not None:
        write_new(output / 'semantic-rubric.json', semantic_rubric)
    report = {'schema_version': 1, 'source': guard.source, 'complete': False, 'stop_reason': None,
              'dataset_sha256': digest(dataset), 'allocation_sha256': digest(plan), 'records': [],
              'evaluator_sha256': evaluator_fingerprint(), 'reserved_model_calls_upper': 0,
              'reserved_cost_upper_microusd': 0, 'quality_evidence': False, 'p12_eligible': False}
    if semantic_hash is not None:
        report['semantic_rubric_sha256'] = semantic_hash
    start = clock()
    try:
        # No policy/profile/threshold update after any answer. The baseline's
        # actual manual answer is reused for paired comparison, never regenerated.
        for strategy in strategies:
            for case in dataset['cases']:
                remaining = plan['run_duration_seconds'] - (clock() - start)
                if remaining < 35: raise TimeoutError('instruction experiment deadline')
                preflight(client, config['runtime'], min(5, remaining))
                selection = config['selections'][strategy]
                request = payload(case, selection, config['runtime'])
                # These prompts do not carry independently reviewed task/tier
                # labels. The classifier must see the original text itself.
                request['requirements']['task'] = 'unknown'
                maximum = 2 if strategy in ('classifier', 'hybrid') else 1
                append_reserved(output / 'core-reservations.jsonl', {'strategy': strategy, 'case_id': case['id'],
                    'evaluation_id': request['evaluation_id'], 'request_sha256': digest(request),
                    'maximum_model_calls': maximum, 'reserved_cost_upper_microusd': request['max_cost_microusd']})
                report['reserved_model_calls_upper'] += maximum
                report['reserved_cost_upper_microusd'] += request['max_cost_microusd']
                if report['reserved_model_calls_upper'] > plan['maximum_generation_calls'] or report['reserved_cost_upper_microusd'] > plan.get('maximum_reserved_cost_microusd',10000000):
                    raise ValueError('whole experiment reserve overflow')
                row = {'case_id': case['id'], 'strategy': strategy, 'evaluation_id': request['evaluation_id'],
                       'verdict': 'unknown', 'contract_status': 'failed', 'target_matches': False}
                began = clock()
                guard.expect(strategy, case, None)
                try:
                    result = client.request('/api/routing/evaluate', request, min(35, remaining))
                    # Reuse existing target, profile, policy and ledger checks.
                    # Its legacy exact-text verdict is replaced only after the
                    # complete Core attestation has passed.
                    row.update(score(result, request, selection, config['runtime'], ''))
                    row.update(assess(constraints[case['id']], result.get('content'), contract_status=row['contract_status'],
                                      finish_reason=row.get('finish_reason'), target_matches=row['target_matches']))
                    if row['contract_status'] == 'completed':
                        append_reserved(output / 'answers.jsonl', {'evaluation_id': request['evaluation_id'],
                            'case_id': case['id'], 'strategy': strategy, 'response': result['content'],
                            'answer_sha256': row['answer_sha256'], 'evaluator_sha256': evaluator_fingerprint()})
                except (OSError, TimeoutError):
                    row['failure'] = 'transport_or_http_refusal'
                except (ValueError, KeyError, TypeError, StopIteration):
                    row['failure'] = 'response_contract_mismatch'
                    report['stop_reason'] = 'response_contract_mismatch'
                finally:
                    guard.clear()
                row.update(elapsed_ms=round((clock() - began) * 1000), source=guard.source)
                report['records'].append(row)
                snapshot(output / 'report.partial.json', report)
                if progress: progress(report)
                if guard.rejections: raise ValueError('native guard contract rejection')
                if hasattr(guard,'halted') and guard.halted(): raise ValueError('provider unavailable; fixed experiment stopped')
                if report['stop_reason']: raise ValueError('instruction contract mismatch')
        report['complete'] = len(report['records']) == expected_primary
    except (ValueError, KeyError, TimeoutError, OSError):
        report['stop_reason'] = report['stop_reason'] or 'instruction_preflight_deadline_or_guard_failed'
    finally:
        report['comparison'] = summarise(dataset, report['records'],strategies)
        write_new(output / 'report.json', report)
    return report
