"""Bound, offline blind semantic review. Never imports or promotes P12 evidence."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import random
import re
import secrets
import sys

from judge_review import digest, encoded, validate_dataset, write_new

ROOT = Path(__file__).resolve().parent
LIMIT = 32 * 1024 * 1024
HASH = re.compile(r'[a-f0-9]{64}\Z')
IDENTITY = re.compile(r'[A-Za-z0-9_.:/-]{1,128}\Z')
OUTCOMES = ('pass', 'fail', 'unknown')


def exact(value, keys, message):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError(message)


def text(value, minimum=1, maximum=2048):
    return isinstance(value, str) and minimum <= len(value.strip()) <= maximum


def identity(value):
    return isinstance(value, str) and bool(IDENTITY.fullmatch(value))


def hash_value(value):
    return isinstance(value, str) and bool(HASH.fullmatch(value))


def read(path, lines=False):
    with Path(path).open('rb') as stream:
        raw = stream.read(LIMIT + 1)
    if len(raw) > LIMIT:
        raise ValueError('review input exceeds 32 MiB')
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('duplicate JSON key')
            result[key] = value
        return result
    def decode(value):
        return json.loads(value, object_pairs_hook=pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-finite JSON')))
    return [decode(line) for line in raw.splitlines() if line.strip()] if lines else decode(raw)


def propose_rubric(dataset, author='machine-proposal', answers_seen=False):
    """Generic starting criteria, deliberately marked machine, not a gold rubric."""
    validate_dataset(dataset)
    descriptions = {
        'correctness': 'The answer contains no material factual or logical error. Unsupported specifics are identified as assumptions; use unknown when verification needs missing sources or expertise.',
        'coverage': 'The answer addresses the substantive request, including supplied context and essential steps, without substituting a different task.',
        'usefulness': 'The answer provides usable, relevant content with enough detail for this request. Length, style or polished wording alone do not establish quality.',
    }
    return {'schema_version': 1, 'dataset_sha256': digest(dataset),
            'author': {'kind': 'machine', 'id': author, 'answers_seen': answers_seen},
            'cases': [{'case_id': case['id'], 'criteria': [
                {'id': key, 'description': value, 'required': True} for key, value in descriptions.items()],
                'references': []} for case in dataset['cases']]}


def validate_rubric(dataset, rubric):
    validate_dataset(dataset)
    exact(rubric, ('schema_version', 'dataset_sha256', 'author', 'cases'), 'invalid rubric fields')
    if type(rubric['schema_version']) is not int or rubric['schema_version'] != 1 or rubric['dataset_sha256'] != digest(dataset):
        raise ValueError('rubric dataset binding mismatch')
    author = rubric['author']
    exact(author, ('kind', 'id', 'answers_seen'), 'invalid rubric author')
    if author['kind'] not in ('human', 'machine') or not identity(author['id']) or type(author['answers_seen']) is not bool:
        raise ValueError('invalid rubric provenance')
    if not isinstance(rubric['cases'], list) or len(rubric['cases']) != len(dataset['cases']):
        raise ValueError('rubric must cover every case')
    expected, found = {c['id'] for c in dataset['cases']}, set()
    for case in rubric['cases']:
        exact(case, ('case_id', 'criteria', 'references'), 'invalid case rubric')
        if case['case_id'] not in expected or case['case_id'] in found:
            raise ValueError('duplicate or foreign rubric case')
        found.add(case['case_id'])
        if not isinstance(case['criteria'], list) or not 1 <= len(case['criteria']) <= 12:
            raise ValueError('rubric criterion bound')
        ids = set()
        for criterion in case['criteria']:
            exact(criterion, ('id', 'description', 'required'), 'invalid criterion fields')
            if (not identity(criterion['id']) or criterion['id'] in ids
                    or not text(criterion['description'], 10) or type(criterion['required']) is not bool):
                raise ValueError('invalid or duplicate criterion')
            ids.add(criterion['id'])
        if not any(c['required'] for c in case['criteria']):
            raise ValueError('at least one criterion must be required')
        if not isinstance(case['references'], list) or len(case['references']) > 16:
            raise ValueError('reference bound')
        for reference in case['references']:
            exact(reference, ('reference', 'snapshot_sha256', 'excerpt'), 'invalid reference')
            if (not text(reference['reference'], maximum=512) or not hash_value(reference['snapshot_sha256'])
                    or not text(reference['excerpt'], maximum=8192)
                    or reference['snapshot_sha256'] != hashlib.sha256(reference['excerpt'].encode('utf-8')).hexdigest()):
                raise ValueError('invalid reference binding')
    if len(encoded(rubric)) > LIMIT:
        raise ValueError('rubric too large')
    return rubric


def bind_run_rubric(dataset, allocation, rubric):
    """Call before the first reservation. No rubric is sent to any model."""
    expected = allocation.get('semantic_rubric_sha256')
    if expected is None and rubric is None:
        return None
    if rubric is None or not hash_value(expected):
        raise ValueError('missing frozen semantic rubric')
    validate_rubric(dataset, rubric)
    if expected != digest(rubric) or rubric['author']['answers_seen']:
        raise ValueError('semantic rubric changed or answers already seen')
    return expected


def inputs(dataset, rubric, report, allocation, answers, baseline):
    validate_rubric(dataset, rubric)
    strategies = allocation.get('strategies')
    if (not isinstance(strategies, list) or not 1 <= len(strategies) <= 8
            or not all(identity(s) for s in strategies) or len(set(strategies)) != len(strategies)
            or baseline not in strategies or len(strategies) * len(dataset['cases']) > 4000):
        raise ValueError('invalid frozen strategy/baseline cohort')
    if (report.get('schema_version') != 1 or allocation.get('dataset_sha256') != digest(dataset)
            or report.get('dataset_sha256') != digest(dataset) or report.get('allocation_sha256') != digest(allocation)
            or not hash_value(report.get('evaluator_sha256'))
            or report['evaluator_sha256'] != allocation.get('evaluator_sha256')
            or report.get('source') not in ('live', 'synthetic') or not isinstance(report.get('records'), list)
            or len(report['records']) > 4000 or not isinstance(answers, list) or len(answers) > 4000):
        raise ValueError('run input binding mismatch')
    cases = {case['id'] for case in dataset['cases']}
    rows, response_rows, evaluations = {}, {}, set()
    for row in report['records']:
        if not isinstance(row, dict):
            raise ValueError('invalid run row')
        key = (row.get('case_id'), row.get('strategy'))
        evaluation = row.get('evaluation_id')
        if (key[0] not in cases or key[1] not in strategies or key in rows
                or not identity(evaluation) or evaluation in evaluations or row.get('source') != report['source']):
            raise ValueError('duplicate/foreign run row or evaluation')
        rows[key] = row
        evaluations.add(evaluation)
    for answer in answers:
        exact(answer, ('evaluation_id', 'case_id', 'strategy', 'response', 'answer_sha256', 'evaluator_sha256'), 'invalid answer fields')
        key = (answer['case_id'], answer['strategy'])
        row = rows.get(key)
        response = answer['response']
        if (key in response_rows or row is None or answer['evaluation_id'] != row['evaluation_id']
                or answer['evaluator_sha256'] != report['evaluator_sha256']
                or not isinstance(response, str) or len(response.encode('utf-8')) > 65536
                or answer['answer_sha256'] != hashlib.sha256(response.encode('utf-8')).hexdigest()
                or answer['answer_sha256'] != row.get('answer_sha256') or row.get('contract_status') != 'completed'):
            raise ValueError('answer hash or Core attestation mismatch')
        response_rows[key] = answer
    result = []
    for case in dataset['cases']:
        for strategy in strategies:
            key = (case['id'], strategy)
            row, answer = rows.get(key), response_rows.get(key)
            if row is None:
                reason = 'not_attempted'
            elif row.get('contract_status') != 'completed' or row.get('target_matches') is not True or row.get('finish_reason') != 'stop':
                reason = 'unattested_or_incomplete'
            elif answer is None:
                reason = 'answer_missing'
            else:
                reason = None
            result.append({'case_id': case['id'], 'strategy': strategy,
                           'evaluation_id': row['evaluation_id'] if row else None,
                           'unavailable_reason': reason, 'response': answer['response'] if reason is None else None})
    return result


def prepare(dataset, rubric, report, allocation, answers, baseline, mode):
    if mode not in ('predeclared', 'retrospective'):
        raise ValueError('invalid review mode')
    slots = inputs(dataset, rubric, report, allocation, answers, baseline)
    predeclared = (allocation.get('semantic_rubric_sha256') == digest(rubric)
                   and report.get('semantic_rubric_sha256') == digest(rubric) and not rubric['author']['answers_seen'])
    if mode == 'predeclared' and not predeclared:
        raise ValueError('rubric was not frozen by the run before inference')
    # Randomise independently within each case. The key is never embedded in HTML.
    random.SystemRandom().shuffle(slots)
    order = {case['id']: i for i, case in enumerate(dataset['cases'])}
    slots.sort(key=lambda slot: order[slot['case_id']])
    cases = {case['id']: case for case in dataset['cases']}
    rubrics = {case['case_id']: case for case in rubric['cases']}
    public_items, private_items = [], []
    used = set()
    for slot in slots:
        blind_id = secrets.token_hex(16)
        while blind_id in used:
            blind_id = secrets.token_hex(16)
        used.add(blind_id)
        case, rule = cases[slot['case_id']], rubrics[slot['case_id']]
        public_items.append({'blind_id': blind_id, 'case_id': case['id'], 'prompt': case['conversation'],
                             'criteria': rule['criteria'], 'references': rule['references'],
                             'available': slot['unavailable_reason'] is None, 'response': slot['response']})
        private_items.append({key: value for key, value in {'blind_id': blind_id, **slot}.items() if key != 'response'})
    public = {'schema_version': 1, 'mode': mode, 'rubric_author_kind': rubric['author']['kind'],
              'rubric_answers_seen': rubric['author']['answers_seen'], 'items': public_items}
    private = {'schema_version': 1, 'dataset': dataset, 'rubric': rubric, 'report': report, 'allocation': allocation,
               'answers': answers, 'baseline': baseline, 'items': private_items}
    manifest = {'schema_version': 1, 'public_sha256': digest(public), 'private_sha256': digest(private),
                'dataset_sha256': digest(dataset), 'rubric_sha256': digest(rubric), 'report_sha256': digest(report),
                'allocation_sha256': digest(allocation), 'answers_sha256': digest(answers)}
    review = {'schema_version': 1, 'bundle_sha256': digest(manifest),
              'reviewer': {'kind': 'human', 'id': '', 'blinded_to_metadata': False}, 'complete': False,
              'items': [{'blind_id': item['blind_id'], 'ratings': [
                  {'criterion_id': c['id'], 'outcome': 'unknown' if not item['available'] else None,
                   'rationale': 'No complete attested answer is available.' if not item['available'] else ''}
                  for c in item['criteria']]} for item in public_items]}
    return public, private, manifest, review


def validate_bundle(public, private, manifest):
    if any(len(encoded(value)) > LIMIT for value in (public, private, manifest)):
        raise ValueError('review bundle exceeds bound')
    exact(manifest, ('schema_version', 'public_sha256', 'private_sha256', 'dataset_sha256', 'rubric_sha256',
                     'report_sha256', 'allocation_sha256', 'answers_sha256'), 'invalid bundle manifest')
    if type(manifest['schema_version']) is not int or manifest['schema_version'] != 1:
        raise ValueError('invalid manifest schema')
    for name, value in [('public', public), ('private', private), *[(name, private[name]) for name in ('dataset', 'rubric', 'report', 'allocation', 'answers')]]:
        if manifest[name + '_sha256'] != digest(value):
            raise ValueError('review bundle changed')
    exact(private, ('schema_version', 'dataset', 'rubric', 'report', 'allocation', 'answers', 'baseline', 'items'), 'invalid private binding')
    exact(public, ('schema_version', 'mode', 'rubric_author_kind', 'rubric_answers_seen', 'items'), 'invalid public binding')
    if type(private['schema_version']) is not int or type(public['schema_version']) is not int or private['schema_version'] != 1 or public['schema_version'] != 1:
        raise ValueError('invalid bundle schema')
    expected = inputs(private['dataset'], private['rubric'], private['report'], private['allocation'], private['answers'], private['baseline'])
    rubric = private['rubric']
    if public['mode'] not in ('predeclared', 'retrospective') or public['rubric_author_kind'] != rubric['author']['kind'] or public['rubric_answers_seen'] != rubric['author']['answers_seen']:
        raise ValueError('rubric provenance changed')
    if public['mode'] == 'predeclared':
        if (bind_run_rubric(private['dataset'], private['allocation'], rubric) != digest(rubric)
                or private['report'].get('semantic_rubric_sha256') != digest(rubric)):
            raise ValueError('run rubric binding changed')
    if not isinstance(private['items'], list) or not isinstance(public['items'], list) or len(private['items']) != len(expected) or len(public['items']) != len(expected):
        raise ValueError('missing review slots')
    by_key = {(s['case_id'], s['strategy']): s for s in expected}
    cases = {c['id']: c for c in private['dataset']['cases']}
    rubrics = {c['case_id']: c for c in rubric['cases']}
    seen, blind_ids = set(), set()
    for revealed, hidden in zip(public['items'], private['items']):
        exact(hidden, ('blind_id', 'case_id', 'strategy', 'evaluation_id', 'unavailable_reason'), 'invalid hidden slot')
        exact(revealed, ('blind_id', 'case_id', 'prompt', 'criteria', 'references', 'available', 'response'), 'invalid blind slot')
        key = (hidden['case_id'], hidden['strategy'])
        blind_id = hidden['blind_id']
        if key not in by_key or key in seen or not isinstance(blind_id, str) or not re.fullmatch(r'[a-f0-9]{32}', blind_id) or blind_id in blind_ids:
            raise ValueError('duplicate/foreign blind slot')
        seen.add(key); blind_ids.add(blind_id)
        slot, case, rule = by_key[key], cases[key[0]], rubrics[key[0]]
        if hidden != {'blind_id': blind_id, **{k: v for k, v in slot.items() if k != 'response'}} or revealed != {
                'blind_id': blind_id, 'case_id': case['id'], 'prompt': case['conversation'],
                'criteria': rule['criteria'], 'references': rule['references'],
                'available': slot['unavailable_reason'] is None, 'response': slot['response']}:
            raise ValueError('blind response/key mismatch')


def finalise(public, private, manifest, review):
    validate_bundle(public, private, manifest)
    exact(review, ('schema_version', 'bundle_sha256', 'reviewer', 'complete', 'items'), 'invalid review fields')
    if type(review['schema_version']) is not int or review['schema_version'] != 1 or review['bundle_sha256'] != digest(manifest) or review['complete'] is not True:
        raise ValueError('review incomplete or foreign bundle')
    reviewer = review['reviewer']
    exact(reviewer, ('kind', 'id', 'blinded_to_metadata'), 'invalid reviewer fields')
    if reviewer['kind'] not in ('human', 'machine') or not identity(reviewer['id']) or type(reviewer['blinded_to_metadata']) is not bool:
        raise ValueError('invalid reviewer provenance')
    if reviewer['kind'] == 'human' and not reviewer['blinded_to_metadata']:
        raise ValueError('human review requires metadata blinding attestation')
    if not isinstance(review['items'], list) or len(review['items']) != len(public['items']):
        raise ValueError('review must cover every slot')
    expected = {item['blind_id']: item for item in public['items']}
    outcomes, seen = {}, set()
    for reviewed in review['items']:
        exact(reviewed, ('blind_id', 'ratings'), 'invalid reviewed slot')
        blind_id = reviewed['blind_id']
        if blind_id not in expected or blind_id in seen:
            raise ValueError('duplicate/foreign review slot')
        seen.add(blind_id)
        item = expected[blind_id]
        criteria = {c['id']: c for c in item['criteria']}
        if not isinstance(reviewed['ratings'], list) or len(reviewed['ratings']) != len(criteria):
            raise ValueError('missing criterion ratings')
        ratings = {}
        for rating in reviewed['ratings']:
            exact(rating, ('criterion_id', 'outcome', 'rationale'), 'invalid rating fields')
            key = rating['criterion_id']
            if key not in criteria or key in ratings or rating['outcome'] not in OUTCOMES or not text(rating['rationale'], 10):
                raise ValueError('invalid or duplicate criterion rating')
            if not item['available'] and rating['outcome'] != 'unknown':
                raise ValueError('unavailable answer must remain unknown')
            ratings[key] = rating['outcome']
        required = [ratings[c['id']] for c in item['criteria'] if c['required']]
        # A proven content failure is not erased by another unknown criterion.
        outcomes[blind_id] = 'fail' if 'fail' in required else 'unknown' if 'unknown' in required else 'pass'
    strategies = private['allocation']['strategies']
    cases = private['dataset']['cases']
    indexed = {(item['case_id'], item['strategy']): outcomes[item['blind_id']] for item in private['items']}
    groups, paired = [], []
    for split in ('calibration', 'test'):
        cohort = [case for case in cases if case['split'] == split]
        if not cohort:
            continue
        for strategy in strategies:
            counts = Counter(indexed[(case['id'], strategy)] for case in cohort)
            families = {case['family_id'] for case in cohort}
            groups.append({'strategy': strategy, 'split': split, 'cases': len(cohort), **{key: counts[key] for key in OUTCOMES},
                           'fully_passed_candidate_groups': sum(all(indexed[(case['id'], strategy)] == 'pass'
                               for case in cohort if case['family_id'] == family) for family in families)})
            if strategy == private['baseline']:
                continue
            cells = Counter((indexed[(c['id'], private['baseline'])], indexed[(c['id'], strategy)]) for c in cohort)
            paired.append({'strategy': strategy, 'baseline': private['baseline'], 'split': split, 'cases': len(cohort),
                           'cells': [{'baseline_outcome': a, 'candidate_outcome': b, 'count': cells[(a, b)]}
                                     for a in OUTCOMES for b in OUTCOMES]})
    return {'schema_version': 1, 'bundle_sha256': digest(manifest), 'review_sha256': digest(review),
            'mode': public['mode'], 'source': private['report']['source'], 'rubric_author': private['rubric']['author'],
            'reviewer': reviewer, 'review_status': 'human_attested' if reviewer['kind'] == 'human' else 'machine_diagnostic',
            'groups': groups, 'paired': paired, 'baseline_response_regenerated': False,
            'semantic_review_complete': True, 'family_independence_reviewed': False,
            'workload_requirements_reviewed': False, 'source_rubric_consistency_reviewed': False,
            'actual_savings_microusd': None, 'quality_evidence': False, 'p12_eligible': False,
            'model_calls': 0, 'policy_updated': False}


def prepare_files(output, dataset, rubric, report, allocation, answers, baseline, mode):
    values = prepare(dataset, rubric, report, allocation, answers, baseline, mode)
    public, private, manifest, review = values
    validate_bundle(public, private, manifest)
    template = (ROOT / 'semantic_review.html').read_text(encoding='utf-8')
    payload = encoded({'public': public, 'review': review}).decode('utf-8').replace('&', '\\u0026').replace('<', '\\u003c').replace('>', '\\u003e')
    if len(payload.encode('utf-8')) > LIMIT:
        raise ValueError('public review page exceeds bound')
    output = Path(output)
    output.mkdir(exist_ok=False)
    for name, value in zip(('public', 'private-key', 'manifest', 'review-draft'), values):
        write_new(output / (name + '.json'), value)
    with (output / 'review.html').open('x', encoding='utf-8') as stream:
        stream.write(template.replace('SEMANTIC_REVIEW_PAYLOAD', payload))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    propose = commands.add_parser('propose-rubric')
    propose.add_argument('--dataset', required=True); propose.add_argument('--output', required=True)
    propose.add_argument('--answers-seen', action='store_true')
    freeze = commands.add_parser('freeze-rubric')
    freeze.add_argument('--dataset', required=True); freeze.add_argument('--rubric', required=True); freeze.add_argument('--output', required=True)
    prepare_command = commands.add_parser('prepare')
    for name in ('dataset', 'rubric', 'report', 'allocation', 'answers', 'baseline', 'output'):
        prepare_command.add_argument('--' + name, required=True)
    prepare_command.add_argument('--mode', choices=('predeclared', 'retrospective'), required=True)
    finalise_command = commands.add_parser('finalise')
    for name in ('bundle', 'review', 'output'):
        finalise_command.add_argument('--' + name, required=True)
    args = parser.parse_args()
    if args.command == 'propose-rubric':
        value = propose_rubric(read(args.dataset), answers_seen=args.answers_seen)
        write_new(args.output, value)
    elif args.command == 'freeze-rubric':
        value = validate_rubric(read(args.dataset), read(args.rubric))
        write_new(args.output, value)
    elif args.command == 'prepare':
        value = prepare_files(args.output, read(args.dataset), read(args.rubric), read(args.report), read(args.allocation), read(args.answers, True), args.baseline, args.mode)
    else:
        directory = Path(args.bundle)
        value = finalise(read(directory / 'public.json'), read(directory / 'private-key.json'), read(directory / 'manifest.json'), read(args.review))
        write_new(args.output, value)
    print(json.dumps({'command': args.command, 'output_sha256': digest(value), 'model_calls': 0,
                      'quality_evidence': False, 'p12_eligible': False, 'policy_updated': False}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, TypeError, KeyError, OSError, RecursionError):
        print('Semantic review refused: invalid binding, completeness, bounds or output path.', file=sys.stderr)
        sys.exit(2)
