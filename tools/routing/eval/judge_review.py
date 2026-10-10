"""Freeze judge labels for blind human review; offline, with no evidence promotion."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import unicodedata

from judge_diagnostics import FLAGS, TASKS, TIERS, analyse

ROOT = Path(__file__).resolve().parent
LIMIT = 4 * 1024 * 1024
_ID = re.compile(r"[A-Za-z0-9_.:/-]{1,128}\Z")
_HASH = re.compile(r"[a-f0-9]{64}\Z")


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')


def digest(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def prompt_digest(text):
    # Conservative duplicate detection, including formatting/case-only rewrites.
    return hashlib.sha256(' '.join(unicodedata.normalize('NFC', text).casefold().split()).encode('utf-8')).hexdigest()


def _object(value, keys, error):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError(error)


def _text(value, minimum=1, maximum=128):
    return isinstance(value, str) and minimum <= len(value.strip()) <= maximum


def _identifier(value):
    return isinstance(value, str) and bool(_ID.fullmatch(value))


def load(path):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('duplicate JSON key')
            result[key] = value
        return result
    with Path(path).open('rb') as stream:
        raw = stream.read(LIMIT + 1)
    if len(raw) > LIMIT:
        raise ValueError('input exceeds 4 MiB')
    return json.loads(raw, object_pairs_hook=pairs,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-finite JSON value')))


def validate_dataset(value):
    _object(value, ('schema_version', 'dataset_id', 'source', 'cases'), 'invalid dataset fields')
    if type(value['schema_version']) is not int or value['schema_version'] != 1 or not _identifier(value['dataset_id']):
        raise ValueError('invalid dataset identity')
    source = value['source']
    _object(source, ('kind', 'reference'), 'invalid source fields')
    if source['kind'] not in ('synthetic', 'independent') or not _text(source['reference'], maximum=512):
        raise ValueError('invalid source declaration')
    cases = value['cases']
    if not isinstance(cases, list) or not 1 <= len(cases) <= 1000:
        raise ValueError('case count outside bounds')
    ids, families, prompts = set(), {}, {}
    for case in cases:
        _object(case, ('id', 'family_id', 'split', 'language', 'conversation'), 'invalid case fields')
        if not _identifier(case['id']) or case['id'] in ids or not _identifier(case['family_id']):
            raise ValueError('invalid or duplicate case identity')
        if case['split'] not in ('calibration', 'test') or case['language'] not in ('en', 'ru'):
            raise ValueError('invalid case split or language')
        if not _text(case['conversation'], maximum=32000) or len(case['conversation'].encode('utf-8')) > 64000:
            raise ValueError('invalid conversation bound')
        family, split = case['family_id'], case['split']
        if family in families and families[family] != split:
            raise ValueError('family leakage between splits')
        fingerprint = prompt_digest(case['conversation'])
        if fingerprint in prompts:
            raise ValueError('duplicate conversation, including normalised rewrites')
        ids.add(case['id'])
        families[family] = split
        prompts[fingerprint] = family
    if len(encoded(value)) > LIMIT:
        raise ValueError('dataset exceeds 4 MiB')
    return value


def seed_exposure(seed):
    # Existing seed is diagnostic/calibration, even if an old label says test.
    result = []
    for case in seed['cases']:
        result.append({'family_id':case['label']['family_id'], 'prompt_sha256':prompt_digest(case['conversation'])})
    return result


def exposure_records(value):
    if isinstance(value, dict) and set(value) == {'schema_version', 'records'}:
        if type(value['schema_version']) is not int or value['schema_version'] != 1 or not isinstance(value['records'], list) or len(value['records']) > 10000:
            raise ValueError('invalid exposure registry')
        for record in value['records']:
            _object(record, ('family_id', 'prompt_sha256'), 'invalid exposure fields')
            if not _identifier(record['family_id']) or not isinstance(record['prompt_sha256'], str) or not _HASH.fullmatch(record['prompt_sha256']):
                raise ValueError('invalid exposure identity')
        return value['records']
    dataset = validate_dataset(value)
    return [{'family_id':c['family_id'], 'prompt_sha256':prompt_digest(c['conversation'])} for c in dataset['cases']]


def freeze(dataset, exposures=()):
    validate_dataset(dataset)
    records, exposure_hashes = [], []
    seed = ROOT/'fixtures/judge-calibration.v1.json'
    if seed.is_file():
        value = load(seed)
        records.extend(seed_exposure(value))
        exposure_hashes.append(digest(value))
    for value in exposures:
        records.extend(exposure_records(value))
        exposure_hashes.append(digest(value))
    exposed_families = {r['family_id'] for r in records}
    exposed_prompts = {r['prompt_sha256'] for r in records}
    for case in dataset['cases']:
        if case['split'] == 'test' and (case['family_id'] in exposed_families or prompt_digest(case['conversation']) in exposed_prompts):
            raise ValueError('held-out case overlaps known exposure')
    manifest = {'schema_version':1, 'dataset_sha256':digest(dataset),
                'source_kind':dataset['source']['kind'], 'exposure_sha256':sorted(set(exposure_hashes)),
                'families':[{'id':family, 'split':split, 'case_ids':sorted(c['id'] for c in dataset['cases'] if c['family_id']==family)}
                            for family, split in sorted({(c['family_id'], c['split']) for c in dataset['cases']})]}
    form = {'schema_version':1, 'dataset_sha256':manifest['dataset_sha256'], 'manifest_sha256':digest(manifest),
            'reviewer_id':'', 'review_kind':'human', 'blinded_to_observations':False, 'complete':False,
            'case_reviews':[{'case_id':c['id'], 'label':{'tasks':[], 'minimum_tier':None, **{flag:None for flag in FLAGS}}, 'rationale':''}
                            for c in dataset['cases']]}
    return {'dataset':dataset, 'manifest':manifest, 'review':form}


def bundle(directory):
    directory = Path(directory)
    dataset, manifest = load(directory/'dataset.json'), load(directory/'manifest.json')
    validate_dataset(dataset)
    _object(manifest, ('schema_version', 'dataset_sha256', 'source_kind', 'exposure_sha256', 'families'), 'invalid manifest fields')
    if manifest['schema_version'] != 1 or manifest['dataset_sha256'] != digest(dataset) or manifest['source_kind'] != dataset['source']['kind']:
        raise ValueError('frozen dataset hash or source mismatch')
    expected = [{'id':family, 'split':split, 'case_ids':sorted(c['id'] for c in dataset['cases'] if c['family_id']==family)}
                for family, split in sorted({(c['family_id'], c['split']) for c in dataset['cases']})]
    if manifest['families'] != expected or not isinstance(manifest['exposure_sha256'], list) or any(not isinstance(v,str) or not _HASH.fullmatch(v) for v in manifest['exposure_sha256']):
        raise ValueError('invalid frozen family or exposure manifest')
    return dataset, manifest


def finalise(dataset, manifest, review):
    # This verifies binding/completeness, not the truth of a human attestation.
    _object(review, ('schema_version', 'dataset_sha256', 'manifest_sha256', 'reviewer_id', 'review_kind',
                    'blinded_to_observations', 'complete', 'case_reviews'), 'invalid review fields')
    if (type(review['schema_version']) is not int or review['schema_version'] != 1 or review['dataset_sha256'] != digest(dataset)
            or review['manifest_sha256'] != digest(manifest) or review['review_kind'] != 'human'
            or review['blinded_to_observations'] is not True or review['complete'] is not True
            or not _identifier(review['reviewer_id'])):
        raise ValueError('review is incomplete, unbound or not human-attested blind review')
    reviews = review['case_reviews']
    if not isinstance(reviews, list) or len(reviews) != len(dataset['cases']):
        raise ValueError('every frozen case needs one review')
    indexed = {}
    for item in reviews:
        _object(item, ('case_id', 'label', 'rationale'), 'invalid review item fields')
        if not _identifier(item['case_id']) or item['case_id'] in indexed or not _text(item['rationale'], minimum=10, maximum=2048):
            raise ValueError('duplicate review or missing rationale')
        label = item['label']
        _object(label, ('tasks', 'minimum_tier', *FLAGS), 'invalid label fields')
        tasks = label['tasks']
        if (not isinstance(tasks, list) or not 1<=len(tasks)<=len(TASKS) or any(not isinstance(t,str) or t not in TASKS for t in tasks)
                or len(set(tasks)) != len(tasks) or label['minimum_tier'] not in TIERS or any(type(label[f]) is not bool for f in FLAGS)
                or 'unknown' in tasks and (tasks != ['unknown'] or not label['uncertain'])):
            raise ValueError('invalid or incomplete expected requirements')
        indexed[item['case_id']] = label
    if set(indexed) != {c['id'] for c in dataset['cases']}:
        raise ValueError('foreign or missing review case')
    return {'schema_version':1,
            'review':{'status':'human_attested', 'reviewer_id':review['reviewer_id'], 'dataset_sha256':digest(dataset),
                      'manifest_sha256':digest(manifest), 'review_sha256':digest(review), 'source_kind':dataset['source']['kind'],
                      'blinded_to_observations':True},
            'quality_evidence':False,
            'cases':[{'id':c['id'], 'family_id':c['family_id'], 'split':c['split'], **indexed[c['id']]} for c in dataset['cases']]}


def write_new(path, value):
    with Path(path).open('xb') as output:
        output.write(encoded(value)+b'\n')


def prepare_files(dataset_path, output, exposed=()):
    frozen = freeze(load(dataset_path), [load(p) for p in exposed])
    template = (ROOT/'judge_review.html').read_text(encoding='utf-8')
    # JSON stays inert even when a prompt contains closing script tags or HTML.
    payload = encoded(frozen).decode('utf-8').replace('&','\\u0026').replace('<','\\u003c').replace('>','\\u003e')
    page = template.replace('FROZEN_REVIEW_PAYLOAD',payload)
    output = Path(output)
    output.mkdir(parents=False, exist_ok=False)
    for name in ('dataset', 'manifest'):
        write_new(output/f'{name}.json', frozen[name])
    write_new(output/'review-draft.json',frozen['review'])
    with (output/'review.html').open('x', encoding='utf-8') as document:
        document.write(page)
    return frozen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    prepare = commands.add_parser('prepare')
    prepare.add_argument('--dataset', required=True); prepare.add_argument('--output', required=True)
    prepare.add_argument('--exposed', action='append', default=[])
    for name in ('finalise', 'report'):
        command = commands.add_parser(name)
        command.add_argument('--bundle', required=True); command.add_argument('--review', required=True)
        command.add_argument('--output', required=True)
        if name == 'report':
            command.add_argument('--observations', required=True)
            command.add_argument('--min-confidence', type=float, default=.75)
    args = parser.parse_args()
    if args.command == 'prepare':
        frozen = prepare_files(args.dataset,args.output,args.exposed)
        result = {'case_count':len(frozen['dataset']['cases']), 'dataset_sha256':frozen['manifest']['dataset_sha256'], 'review_complete':False}
    else:
        dataset, manifest = bundle(args.bundle)
        labels = finalise(dataset,manifest,load(args.review))
        result = labels if args.command == 'finalise' else analyse(labels,load(args.observations),args.min_confidence)
        if args.command == 'report':
            result['label_review'] = labels['review']
        write_new(args.output,result)
        result = {'case_count':len(labels['cases']), 'review_complete':True, 'source_kind':dataset['source']['kind']}
    print(json.dumps({**result,'model_calls':0,'policy_updated':False,'quality_evidence':False}))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, TypeError, KeyError, OSError):
        print('Review operation refused: invalid input, binding, completeness or output path.',file=sys.stderr)
        sys.exit(2)
