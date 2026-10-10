"""Use the existing Core attestation path for a frozen formal final-value comparison."""
from pathlib import Path
import time

from audited_run import append_reserved, payload, preflight, score, snapshot
from judge_review import digest, write_new
from objective_answers import assess, fingerprint, summarise, validate

STRATEGIES = ('openai', 'gemini-lite', 'classifier', 'hybrid')


def execute(dataset, bank, config, plan, output, client, guard, *, clock=time.monotonic, progress=None):
    validate(dataset, bank)
    if (len(dataset['cases']) != 20 or tuple(plan.get('strategies', ())) != STRATEGIES
            or plan.get('dataset_sha256') != digest(dataset) or plan.get('answer_bank_sha256') != digest(bank)
            or plan.get('evaluator_sha256') != fingerprint() or plan.get('maximum_primary_calls') != 80
            or plan.get('maximum_generation_calls') != 120 or plan.get('baseline') != 'gemini-lite'):
        raise ValueError('exact frozen formal pilot shape required')
    output = Path(output); output.mkdir(exist_ok=False); write_new(output / 'allocation.json', plan)
    report = {'schema_version': 1, 'source': guard.source, 'complete': False, 'stop_reason': None, 'records': [],
              'dataset_sha256': digest(dataset), 'allocation_sha256': digest(plan), 'evaluator_sha256': fingerprint(),
              'reserved_model_calls_upper': 0, 'reserved_cost_upper_microusd': 0, 'quality_evidence': False, 'p12_eligible': False}
    references = {r['case_id']: r for r in bank['cases']}; start = clock()
    try:
        for strategy in STRATEGIES:
            for case in dataset['cases']:
                remaining = plan['run_duration_seconds'] - (clock() - start)
                if remaining < 35: raise TimeoutError('formal experiment deadline')
                preflight(client, config['runtime'], min(5, remaining))
                selection = config['selections'][strategy]; request = payload(case, selection, config['runtime'])
                request['requirements']['task'] = 'unknown'  # No unreviewed tier/task labels sent as routing hints.
                maximum = 2 if strategy in ('classifier', 'hybrid') else 1
                append_reserved(output / 'core-reservations.jsonl', {'strategy': strategy, 'case_id': case['id'],
                    'evaluation_id': request['evaluation_id'], 'request_sha256': digest(request),
                    'maximum_model_calls': maximum, 'reserved_cost_upper_microusd': request['max_cost_microusd']})
                report['reserved_model_calls_upper'] += maximum; report['reserved_cost_upper_microusd'] += request['max_cost_microusd']
                if report['reserved_model_calls_upper'] > 120 or report['reserved_cost_upper_microusd'] > plan['maximum_reserved_cost_microusd']:
                    raise ValueError('formal whole allocation exceeded')
                row = {'case_id': case['id'], 'strategy': strategy, 'evaluation_id': request['evaluation_id'],
                       'verdict': 'unknown', 'contract_status': 'failed', 'target_matches': False}
                began = clock(); guard.expect(strategy, case, None)
                try:
                    result = client.request('/api/routing/evaluate', request, min(35, remaining))
                    row.update(score(result, request, selection, config['runtime'], ''))
                    row.update(assess(references[case['id']], result.get('content'), contract_status=row['contract_status'],
                                      finish_reason=row.get('finish_reason'), target_matches=row['target_matches']))
                    if row['contract_status'] == 'completed':
                        append_reserved(output / 'answers.jsonl', {'evaluation_id': request['evaluation_id'], 'case_id': case['id'],
                            'strategy': strategy, 'response': result['content'], 'answer_sha256': row['answer_sha256'], 'evaluator_sha256': fingerprint()})
                except (OSError, TimeoutError): row['failure'] = 'transport_or_http_refusal'
                except (ValueError, KeyError, TypeError, StopIteration):
                    row['failure'] = 'response_contract_mismatch'; report['stop_reason'] = 'response_contract_mismatch'
                finally: guard.clear()
                row.update(elapsed_ms=round((clock() - began) * 1000), source=guard.source)
                report['records'].append(row); snapshot(output / 'report.partial.json', report)
                if progress: progress(report)
                if guard.rejections or guard.halted() or report['stop_reason']: raise ValueError('formal guard/availability stopped')
        report['complete'] = len(report['records']) == 80
    except (ValueError, KeyError, TimeoutError, OSError):
        report['stop_reason'] = report['stop_reason'] or 'formal_preflight_deadline_or_guard_failed'
    finally:
        report['comparison'] = summarise(dataset, bank, report['records'], STRATEGIES, plan['baseline'])
        write_new(output / 'report.json', report)
    return report
