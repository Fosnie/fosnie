"""Bounded audited-cohort evaluation through the protected Core text endpoint."""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from cohort_report import lower_bound, summarise
from judge_review import digest, encoded, load, write_new
from reference_audit import check

CONTRACT = 'completed_text_v1'
PROFILE_FIELDS = {'provider_id', 'provider_revision', 'revision', 'adapter', 'model', 'locality',
                  'locality_trusted', 'capabilities', 'tier', 'task_tiers', 'quality_status',
                  'evaluation_samples', 'evaluation_version', 'pricing', 'latency_ms',
                  'auto_enabled', 'auto_effort', 'task_exclusions', 'quality_evidence'}


def exact(value, fields):
    if not isinstance(value, dict) or set(value) != set(fields):
        raise ValueError('unexpected configuration fields')


def positive(value, maximum=10**12):
    if type(value) is not int or not 1 <= value <= maximum:
        raise ValueError('bounded positive integer required')


def identity(value):
    if not isinstance(value, str) or str(uuid.UUID(value)) != value:
        raise ValueError('canonical UUID required')


def hash_value(value):
    if not isinstance(value, str) or not re.fullmatch('[a-f0-9]{64}', value):
        raise ValueError('SHA256 required')


def read_config(path):
    value = load(path)
    exact(value, {'schema_version', 'dataset_sha256', 'base_url', 'auth_env', 'local_only',
                  'limits', 'case_ids', 'profiles', 'policies', 'configurations'})
    if type(value['schema_version']) is not int or value['schema_version'] != 1:
        raise ValueError('configuration version')
    hash_value(value['dataset_sha256'])
    url = urllib.parse.urlsplit(value['base_url'])
    if (url.username or url.password or url.query or url.fragment or url.path not in ('', '/')
        or not url.hostname or url.port == 0 or any(c.isspace() for c in value['base_url'])
        or url.scheme != 'https' and not (url.scheme == 'http' and url.hostname in ('127.0.0.1', 'localhost', '::1'))):
        raise ValueError('HTTPS or loopback Core origin required')
    if not isinstance(value['auth_env'], str) or not re.fullmatch('[A-Z][A-Z0-9_]{0,63}', value['auth_env']):
        raise ValueError('credential environment name required')
    if type(value['local_only']) is not bool:
        raise ValueError('explicit locality constraint required')
    limits = value['limits']
    exact(limits, {'max_requests', 'max_model_calls', 'total_budget_microusd', 'request_budget_microusd',
                   'max_duration_ms', 'run_duration_ms', 'max_output_tokens'})
    for number in limits.values():
        positive(number)
    if (limits['max_requests'] > 1000 or limits['max_model_calls'] > 2000
        or not 50 <= limits['max_duration_ms'] <= 30000
        or not limits['max_duration_ms'] <= limits['run_duration_ms'] <= 3600000
        or limits['max_output_tokens'] > 2048):
        raise ValueError('unsupported run bounds')
    ids = value['case_ids']
    if not isinstance(ids, list) or not 1 <= len(ids) <= 1000 or any(not isinstance(i, str) for i in ids) or len(set(ids)) != len(ids):
        raise ValueError('unique ordered cases required')
    profiles = value['profiles']
    if not isinstance(profiles, list) or not 1 <= len(profiles) <= 32:
        raise ValueError('bounded saved profiles required')
    providers = set()
    for profile in profiles:
        exact(profile, {'snapshot', 'profile_sha256'})
        hash_value(profile['profile_sha256'])
        saved = profile['snapshot']
        if not isinstance(saved, dict) or set(saved) - PROFILE_FIELDS or not {'provider_id', 'provider_revision', 'revision', 'model', 'capabilities'} <= set(saved):
            raise ValueError('saved profile schema')
        exclusions = saved.get('task_exclusions', [])
        if (not isinstance(exclusions, list) or len(exclusions)>8
            or any(task not in ('unknown','current_fact','general','editing','extraction','coding','reasoning','research') for task in exclusions)
            or len(set(exclusions))!=len(exclusions)):
            raise ValueError('saved task exclusions')
        capabilities = saved['capabilities']
        if not isinstance(capabilities, dict):raise ValueError('saved capabilities')
        json_efforts = capabilities.get('structured_output_efforts')
        if json_efforts is not None:
            supported = capabilities.get('efforts', [])
            if (not isinstance(json_efforts, list) or len(json_efforts)>16
                or any(not isinstance(e, str) or not e or len(e)>24 or e.strip()!=e for e in json_efforts)
                or len(set(json_efforts))!=len(json_efforts)
                or not isinstance(supported, list) or any(not isinstance(e, str) for e in supported)
                or any(e not in supported for e in json_efforts)):
                raise ValueError('saved JSON output efforts')
        identity(saved['provider_id']);positive(saved['revision']);positive(saved['provider_revision'])
        if saved['provider_id'] in providers or not isinstance(saved['model'], str) or not saved['model'].strip():
            raise ValueError('duplicate or invalid saved profile')
        if value['local_only'] and (saved.get('locality') != 'local' or saved.get('locality_trusted') is not True):
            raise ValueError('local-only executor and judge profiles must be trusted local')
        providers.add(saved['provider_id'])
    policies = value['policies']
    if not isinstance(policies, list) or not 1 <= len(policies) <= 32:
        raise ValueError('bounded saved policy snapshots required')
    by_policy = {}
    for saved in policies:
        if not isinstance(saved, dict) or not {'id', 'revision', 'policy', 'scope', 'is_default'} <= set(saved):
            raise ValueError('saved policy envelope required')
        identity(saved['id']);positive(saved['revision'])
        policy = saved['policy']
        if (saved['id'] in by_policy or not isinstance(policy, dict) or policy.get('id') != saved['id']
            or policy.get('revision') != saved['revision'] or policy.get('rollout') != 'active'
            or policy.get('strategy') not in ('rules', 'classifier', 'hybrid')):
            raise ValueError('active saved policy binding')
        judge = policy.get('classifier_provider_id')
        if judge is not None and judge not in providers:
            raise ValueError('judge profile must be pinned separately from executors')
        by_policy[saved['id']] = saved
    configs = value['configurations']
    if not isinstance(configs, list) or not 1 <= len(configs) <= 16:
        raise ValueError('bounded fixed configurations required')
    seen = set()
    for config in configs:
        exact({k: v for k, v in config.items() if k != 'temperature'}, {'id', 'mode', 'policy_id', 'provider_ids', 'efforts', 'objective'})
        if 'temperature' in config and (type(config['temperature']) not in (int, float) or not math.isfinite(config['temperature']) or not 0 <= config['temperature'] <= 2):
            raise ValueError('finite temperature from0 to2 required')
        if not isinstance(config['id'], str) or not re.fullmatch('[a-zA-Z0-9_.-]{1,64}', config['id']) or config['id'] in seen:
            raise ValueError('unique configuration identity required')
        seen.add(config['id'])
        if config['mode'] not in ('manual', 'auto') or config['policy_id'] not in by_policy or config['objective'] not in ('economy', 'balanced', 'quality'):
            raise ValueError('selection and saved policy required')
        selected = config['provider_ids'];efforts = config['efforts']
        if (not isinstance(selected, list) or not selected or any(p not in providers for p in selected)
            or len(set(selected)) != len(selected) or not isinstance(efforts, list) or not efforts
            or any(e is not None and (not isinstance(e, str) or not re.fullmatch('[a-zA-Z0-9_-]{1,24}', e)) for e in efforts)
            or len(set(efforts)) != len(efforts)):
            raise ValueError('bounded explicit target/effort acceptance set required')
        if config['mode'] == 'manual' and (len(selected) != 1 or len(efforts) != 1 or by_policy[config['policy_id']]['is_default'] is not True):
            raise ValueError('manual needs one target/effort and effective default policy')
    return value


def make_plan(dataset, bank, manifest, config):
    if digest(dataset) != config['dataset_sha256']:
        raise ValueError('configuration dataset mismatch')
    cases = {c['id']: c for c in dataset['cases']}
    if any(i not in cases for i in config['case_ids']):
        raise ValueError('foreign case selection')
    requests = [{'configuration_id': c['id'], 'case_id': case, 'maximum_model_calls': 1 if c['mode'] == 'manual' else 2}
                for c in config['configurations'] for case in config['case_ids']]
    limits = config['limits'];calls = sum(r['maximum_model_calls'] for r in requests)
    reserved = len(requests) * limits['request_budget_microusd']
    if len(requests) > limits['max_requests'] or calls > limits['max_model_calls'] or reserved > limits['total_budget_microusd']:
        raise ValueError('entire fixed plan must fit the approved ceilings')
    return {'schema_version': 1, 'contract_version': CONTRACT, 'configuration_sha256': digest(config),
            'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank), 'manifest_sha256': digest(manifest),
            'endpoint': config['base_url'].rstrip('/') + '/api/routing/evaluate', 'local_only': config['local_only'],
            'limits': limits, 'requests': requests, 'planned_requests': len(requests),
            'maximum_model_calls': calls, 'reserved_cost_upper_microusd': reserved,
            'retry': False, 'resumable': False, 'network_calls': 0, 'quality_evidence': False,
            'fresh_holdout': False, 'exposure_inherited': True, 'policy_updated': False}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError('redirect refused')


class CoreClient:
    def __init__(self, origin, credential):
        if not credential or '\r' in credential or '\n' in credential:
            raise ValueError('Core credential absent or invalid')
        self.origin = origin.rstrip('/');self.credential = credential
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, path, body=None, timeout=5, method=None):
        request = urllib.request.Request(self.origin + path, data=encoded(body) if body is not None else None,
                                         headers={'Authorization': 'Bearer ' + self.credential, 'Content-Type': 'application/json'}, method=method)
        with self.opener.open(request, timeout=timeout) as response:
            raw = response.read(1000001)
        if len(raw) > 1000000:
            raise ValueError('Core response bound')
        def pairs(items):
            result = {}
            for key, value in items:
                if key in result:raise ValueError('duplicate Core response field')
                result[key] = value
            return result
        return json.loads(raw, object_pairs_hook=pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite Core response')))

    def close(self):
        self.credential = None


def preflight(client, config, timeout=5):
    management = client.request('/api/routing/management', timeout=timeout)
    if not isinstance(management, dict) or management.get('deployment_manage') is not True:
        raise ValueError('effective providers.manage required')
    saved = client.request('/api/routing/policies', timeout=timeout)
    catalog = client.request('/api/routing/catalog', timeout=timeout)
    if not isinstance(saved, list) or not isinstance(catalog, list) or max(len(saved), len(catalog)) > 1000:
        raise ValueError('catalogue/policy bounds')
    policies = {row['id']: row for row in saved};entries = {row['id']: row for row in catalog}
    if len(policies) != len(saved) or len(entries) != len(catalog):
        raise ValueError('duplicate catalogue/policy entries')
    default = max((p for p in saved if p.get('is_default') is True), key=lambda p: p.get('scope') == 'user', default=None)
    for snapshot in config['policies']:
        if policies.get(snapshot['id']) != snapshot:
            raise ValueError('policy snapshot changed')
    if any(c['mode'] == 'manual' and (default is None or c['policy_id'] != default['id']) for c in config['configurations']):
        raise ValueError('effective default changed')
    for pinned in config['profiles']:
        profile = pinned['snapshot'];entry = entries.get(profile['provider_id'], {})
        if (entry.get('enabled') is not True or entry.get('profile') != profile
            or entry.get('provider_revision') != profile['provider_revision'] or entry.get('profile_revision') != profile['revision']):
            raise ValueError('provider/profile snapshot changed')
    for selection in config['configurations']:
        if selection['mode'] != 'auto':continue
        policy = policies[selection['policy_id']]['policy']
        possible = {e['id'] for e in catalog if e.get('enabled') is True and isinstance(e.get('profile'), dict)
                    and e['profile'].get('auto_enabled') is True}
        if policy.get('default_provider_id') is not None:possible.add(policy['default_provider_id'])
        possible.update(r['action']['provider_id'] for r in policy.get('rules', []) if r.get('action', {}).get('kind') == 'select')
        allowed = policy.get('constraints', {}).get('allowed_providers')
        if allowed is not None:possible.intersection_update(allowed)
        if not possible <= set(selection['provider_ids']):
            raise ValueError('Auto can select an unpinned executor; narrow the saved policy or acceptance set')
    return {'checked_profiles': len(config['profiles']), 'checked_policies': len(config['policies']),
            'model_calls': 0, 'quality_evidence': False}


def payload(case, selection, config):
    saved = next(p for p in config['policies'] if p['id'] == selection['policy_id']);limits = config['limits']
    intent = {'mode': 'manual', 'provider_id': selection['provider_ids'][0]} if selection['mode'] == 'manual' else {
        'mode': 'auto', 'policy_id': saved['id'], 'objective': selection['objective']}
    requirements = {'task': 'reasoning', 'minimum_tier': 'economy', 'structured_output': False, 'language': case['language']}
    if selection['mode'] == 'manual':requirements['effort'] = selection['efforts'][0]
    sampling = {'temperature': selection['temperature']} if 'temperature' in selection else {}
    return {**sampling, 'evaluation_id': str(uuid.uuid4()), 'policy_id': saved['id'], 'expected_policy_revision': saved['revision'],
            'selection': intent, 'messages': [{'role': 'user', 'content': case['conversation']}], 'requirements': requirements,
            'local_only': config['local_only'], 'max_output_tokens': limits['max_output_tokens'],
            'max_cost_microusd': limits['request_budget_microusd'], 'max_duration_ms': limits['max_duration_ms']}


def score(value, request, selection, config, target):
    if (not isinstance(value, dict) or value.get('evaluation_id') != request['evaluation_id'] or value.get('mode') != 'live'
        or value.get('automatic_replay') is not False):
        raise ValueError('evaluation identity/replay mismatch')
    if value.get('ok') is False:
        return {'contract_status': 'failed', 'verdict': 'unknown', 'target_matches': False,
                'finish_reason': None, 'failure': 'core_failed_or_revoked'}
    if (value.get('ok') is not True or value.get('contract_version') != CONTRACT or value.get('finish_reason') != 'stop'
        or value.get('quality_evaluated') is not False or not isinstance(value.get('content'), str)
        or not value['content'].strip() or len(value['content'].encode('utf-8')) > 64000):
        raise ValueError('completed text attestation absent')
    decision = value['decision'];actual = decision['actual']
    saved = next(p for p in config['policies'] if p['id'] == selection['policy_id'])
    if (decision['policy_id'] != saved['id'] or decision['policy_revision'] != saved['revision']
        or selection['mode'] == 'auto' and decision['strategy'] != saved['policy']['strategy']
        or actual['provider_id'] not in selection['provider_ids']
        or actual['effort'] not in selection['efforts']):
        raise ValueError('actual selection/policy mismatch')
    pinned = next(p for p in config['profiles'] if p['snapshot']['provider_id'] == actual['provider_id']);profile = pinned['snapshot']
    if (actual['model'] != profile['model'] or actual['provider_revision'] != profile['provider_revision']
        or actual['profile_revision'] != profile['revision'] or value.get('profile_sha256') != pinned['profile_sha256']):
        raise ValueError('actual model/profile mismatch')
    usage = value['usage'];judge = value['classifier_ledger'];stages = value['stage_usage']
    ceiling = 1 if selection['mode'] == 'manual' else 2
    if (type(usage['calls']) is not int or not 1 <= usage['calls'] <= ceiling
        or type(judge['calls']) is not int or judge['calls'] != usage['calls'] - 1
        or not isinstance(stages, list) or len(stages) != 1 or stages[0].get('purpose') != 'primary'
        or stages[0].get('provider_id') != actual['provider_id']):
        raise ValueError('unexpected helper/retry/call accounting')
    for ledger in (usage, judge):
        for key in ('cost_microusd', 'tokens', 'unknown_cost_calls', 'unknown_token_calls'):
            if type(ledger.get(key)) is not int or not 0 <= ledger[key] <= 10**12:
                raise ValueError('invalid accounting ledger')
        if ledger['cost_microusd'] > request['max_cost_microusd']:
            raise ValueError('cost exceeded request ceiling')
    # Preserve only bounded accounting fields; never propagate arbitrary upstream text.
    safe_ledger = lambda ledger: {k: ledger[k] for k in ('calls', 'cost_microusd', 'tokens', 'unknown_cost_calls', 'unknown_token_calls')}
    safe_stage = {k: stages[0].get(k) for k in ('purpose', 'provider_id', 'input_tokens', 'output_tokens', 'total_tokens', 'reasoning_tokens', 'cached_input_tokens', 'latency_ms')}
    for key, number in safe_stage.items():
        if key not in ('purpose', 'provider_id') and number is not None and (type(number) is not int or not 0 <= number <= 10**12):
            raise ValueError('invalid stage accounting')
    return {'contract_status': 'completed', 'verdict': 'pass' if value['content'].strip() == target.strip() else 'fail',
            'finish_reason': 'stop', 'terminal_attestation': CONTRACT, 'target_matches': True,
            'actual': {k: actual[k] for k in ('provider_id', 'provider_revision', 'profile_revision', 'model', 'effort')},
            'profile_sha256': pinned['profile_sha256'], 'effort': actual['effort'], 'usage': safe_ledger(usage),
            'classifier_ledger': safe_ledger(judge), 'stage_usage': [safe_stage],
            'answer_sha256': hashlib.sha256(value['content'].encode('utf-8')).hexdigest(), 'answer_chars': len(value['content'])}


def aggregate(dataset, config, records):
    indexed = {(r['configuration_id'], r['case_id']): r for r in records};groups = []
    if len(indexed) != len(records):raise ValueError('duplicate attempt')
    cases = [c for c in dataset['cases'] if c['id'] in config['case_ids']]
    for selection in config['configurations']:
        for split in ('calibration', 'test', 'all'):
            selected = [c for c in cases if split == 'all' or c['split'] == split]
            counts = Counter(indexed.get((selection['id'], c['id']), {}).get('verdict', 'unknown') for c in selected)
            families = {c['family_id'] for c in selected}
            passed = sum(all(indexed.get((selection['id'], c['id']), {}).get('verdict') == 'pass'
                             for c in selected if c['family_id'] == family) for family in families)
            groups.append({'configuration_id': selection['id'], 'mode': selection['mode'], 'split': split,
                           'cases': len(selected), 'pass': counts['pass'], 'fail': counts['fail'], 'unknown': counts['unknown'],
                           'families': len(families), 'passed_families': passed,
                           'wilson95_family_lower_bound': lower_bound(passed, len(families))})
    return groups


def append_reserved(path, value):
    with path.open('ab') as stream:
        stream.write(encoded(value) + b'\n');stream.flush();os.fsync(stream.fileno())


def snapshot(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('wb') as stream:
        stream.write(encoded(value) + b'\n');stream.flush();os.fsync(stream.fileno())
    temporary.replace(path)


def execute(dataset, bank, config, plan, output, client, clock=time.monotonic):
    # New directories only. A interrupted reservation is never refunded/resumed.
    output = Path(output);output.mkdir(exist_ok=False)
    write_new(output / 'allocation.json', plan)
    report = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'allocation_sha256': digest(plan),
              'records': [], 'attempted_requests': 0, 'reserved_model_calls_upper': 0,
              'reserved_cost_upper_microusd': 0, 'complete': False, 'stop_reason': None,
              'actual_cost_microusd': None, 'quality_evidence': False, 'fresh_holdout': False,
              'policy_updated': False, 'automatic_retry': False}
    cases = {c['id']: c for c in dataset['cases']};targets = {c['case_id']: c['target'] for c in bank['cases']}
    configs = {c['id']: c for c in config['configurations']};start = clock();limits = config['limits']
    try:
        for attempt in plan['requests']:
            remaining = limits['run_duration_ms'] / 1000 - (clock() - start)
            if remaining < limits['max_duration_ms'] / 1000 + 1:
                report['stop_reason'] = 'run_deadline';break
            preflight(client, config, min(5, remaining))
            remaining = limits['run_duration_ms'] / 1000 - (clock() - start)
            if remaining < limits['max_duration_ms'] / 1000 + 1:
                report['stop_reason'] = 'run_deadline';break
            selection = configs[attempt['configuration_id']];case = cases[attempt['case_id']]
            request = payload(case, selection, config)
            reservation = {**attempt, 'evaluation_id': request['evaluation_id'], 'request_sha256': digest(request),
                           'reserved_cost_upper_microusd': limits['request_budget_microusd']}
            append_reserved(output / 'reservations.jsonl', reservation)
            report['attempted_requests'] += 1;report['reserved_model_calls_upper'] += attempt['maximum_model_calls']
            report['reserved_cost_upper_microusd'] += limits['request_budget_microusd']
            row = {'configuration_id': selection['id'], 'case_id': case['id'], 'evaluation_id': request['evaluation_id'],
                   'contract_status': 'failed', 'verdict': 'unknown', 'finish_reason': None, 'target_matches': False}
            begin = clock()
            try:
                value = client.request('/api/routing/evaluate', request, min(limits['max_duration_ms'] / 1000 + 5, remaining))
                row.update(score(value, request, selection, config, targets[case['id']]))
            except (urllib.error.URLError, TimeoutError, OSError):
                row['failure'] = 'transport_or_http_refusal'
            except (ValueError, KeyError, TypeError, StopIteration):
                row['failure'] = 'response_contract_mismatch';report['stop_reason'] = 'response_contract_mismatch'
            row['elapsed_ms'] = round((clock() - begin) * 1000)
            report['records'].append(row)
            snapshot(output / 'report.partial.json', report)
            if report['stop_reason']:break
        report['complete'] = len(report['records']) == plan['planned_requests'] and report['stop_reason'] is None
    except (ValueError, KeyError, TypeError, urllib.error.URLError, TimeoutError, OSError):
        report['stop_reason'] = 'preflight_or_persistence_failed'
    finally:
        report['groups'] = aggregate(dataset, config, report['records'])
        write_new(output / 'report.json', report)
        snapshot(output / 'report.partial.json', report)
    return report


def manual_observations(dataset, config, report):
    configs = []
    for selection in config['configurations']:
        if selection['mode'] != 'manual':continue
        pinned = next(p for p in config['profiles'] if p['snapshot']['provider_id'] == selection['provider_ids'][0])
        configs.append({'id': selection['id'], 'model': pinned['snapshot']['model'],
                        'effort': selection['efforts'][0], 'profile_sha256': pinned['profile_sha256']})
    if not configs:return None
    by_id = {c['id']: c for c in configs}
    records = [{**r, 'profile_sha256': by_id[r['configuration_id']]['profile_sha256'],
                'effort': by_id[r['configuration_id']]['effort']} for r in report['records'] if r['configuration_id'] in by_id]
    return {'schema_version': 1, 'dataset_sha256': digest(dataset), 'configurations': configs, 'records': records}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('plan', 'preflight', 'run'))
    for name in ('cohort', 'parent', 'config', 'output'):parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--plan', type=Path);parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    output = args.output.resolve()
    for protected in (args.cohort.resolve(), args.parent.resolve()):
        if output == protected or output.is_relative_to(protected):raise ValueError('output cannot modify source cohort')
    if output.exists():raise ValueError('new output required')
    dataset, bank, manifest, _ = check(args.cohort, args.parent)
    config = read_config(args.config);plan = make_plan(dataset, bank, manifest, config)
    if args.command == 'plan':
        if args.plan or args.execute:raise ValueError('plan is network-free')
        write_new(output, plan)
        print(json.dumps({'status': 'planned', 'requests': plan['planned_requests'], 'maximum_model_calls': plan['maximum_model_calls'], 'network_calls': 0}))
        return 0
    if not args.plan or load(args.plan) != plan:raise ValueError('exact prepared plan required')
    if args.command == 'run' and not args.execute:raise ValueError('explicit --execute required for model calls')
    if args.command == 'preflight' and args.execute:raise ValueError('preflight makes no model calls')
    client = CoreClient(config['base_url'], os.environ.get(config['auth_env']))
    try:
        if args.command == 'preflight':
            receipt = preflight(client, config);output.mkdir(exist_ok=False)
            write_new(output / 'preflight.json', {**receipt, 'allocation_sha256': digest(plan)})
            print(json.dumps({'status': 'preflight_checked', **receipt}));return 0
        report = execute(dataset, bank, config, plan, output, client)
        observations = manual_observations(dataset, config, report)
        if observations:
            write_new(output / 'manual-observations.json', observations)
            write_new(output / 'manual-comparison.json', summarise(dataset, observations))
        print(json.dumps({'status': 'finished', 'complete': report['complete'], 'attempted_requests': report['attempted_requests'],
                          'reserved_model_calls_upper': report['reserved_model_calls_upper'], 'quality_evidence': False}))
        return 0 if report['complete'] else 1
    finally:client.close()


if __name__ == '__main__':
    try:sys.exit(main())
    except (ValueError, OSError, KeyError, TypeError, StopIteration, urllib.error.URLError):
        print('Audited evaluation refused; request contents and credentials are not logged.', file=sys.stderr)
        sys.exit(2)
