"""Freeze public annotated QA from unused document groups; no tier certification."""
from collections import defaultdict
import hashlib
import json
from pathlib import Path

from judge_review import digest, load, validate_dataset, write_new
from qa_span_answers import fingerprint, normalize, prompt, validate

VERSION = 'fosnie-public-span-v41'
REVISION = 'eee5fdbf62f8613a7812b03419e6b29617b74fd1'
SOURCE_LIMIT = 40 * 1024 * 1024


def exposures(root):
    root = Path(root).resolve(); families, ids, files = set(), set(), []
    for path in sorted(root.rglob('*.json')):
        if path.name not in ('dataset.json', 'prompts.json'): continue
        value = load(path)
        if not isinstance(value, dict) or not isinstance(value.get('cases'), list): continue
        found = False
        for case in value['cases']:
            if isinstance(case, dict) and str(case.get('id', '')).startswith('squad/'):
                ids.add(case['id']); families.add(case['family_id']); found = True
        if found: files.append({'path': str(path.relative_to(root)), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
    return {'schema_version': 1, 'case_ids': sorted(ids), 'family_ids': sorted(families), 'files': files}


def source(path, manifest):
    if (manifest.get('revision') != REVISION or manifest.get('repository') != 'rajpurkar/SQuAD-explorer'
            or manifest.get('upstream_split') != 'train' or manifest.get('url') !=
            f'https://raw.githubusercontent.com/rajpurkar/SQuAD-explorer/{REVISION}/dataset/train-v1.1.json'):
        raise ValueError('pinned public QA source required')
    with Path(path).open('rb') as stream: raw = stream.read(SOURCE_LIMIT + 1)
    if len(raw) > SOURCE_LIMIT or len(raw) != manifest['bytes'] or hashlib.sha256(raw).hexdigest() != manifest['sha256']:
        raise ValueError('public QA source byte binding')
    value = json.loads(raw)
    if value.get('version') != '1.1' or not isinstance(value.get('data'), list): raise ValueError('public QA source shape')
    return value, raw


def build(document, source_manifest, exposure):
    groups = defaultdict(list); excluded_ids = set(exposure['case_ids']); excluded_families = set(exposure['family_ids'])
    for article in document['data']:
        title = article['title']; family = 'squad/' + digest(' '.join(title.casefold().split()))
        if family in excluded_families: continue
        for index, paragraph in enumerate(article['paragraphs']):
            context = paragraph['context']
            if not 1 <= len(context) <= 2200: continue
            for question in paragraph['qas']:
                identity = 'squad/' + question['id']; query = question['question']; annotations = question['answers']
                if identity in excluded_ids or not 1 <= len(query) <= 500 or not 1 <= len(annotations) <= 16: continue
                if any(type(a.get('answer_start')) is not int or a['answer_start'] < 0 or not isinstance(a.get('text'), str)
                    or not 1 <= len(a['text']) <= 128 or not normalize(a['text'])
                    or context[a['answer_start']:a['answer_start'] + len(a['text'])] != a['text'] for a in annotations): continue
                conversation = prompt(context, query)
                if len(conversation.encode('utf-8')) > 6000: continue
                groups[family].append({'case_id': identity, 'article': title, 'paragraph_index': index,
                    'question': query, 'context': context, 'answers': annotations,
                    'context_sha256': hashlib.sha256(context.encode('utf-8')).hexdigest()})
    selected = sorted(groups, key=lambda family: digest([VERSION, family]))[:20]
    if len(selected) != 20: raise ValueError('twenty unused public document groups required')
    rows = [min(groups[family], key=lambda row: digest([VERSION, row['case_id'], row['context_sha256']])) for family in selected]
    cases = [{'id': r['case_id'], 'family_id': family, 'split': 'test', 'language': 'en', 'conversation': prompt(r['context'], r['question'])}
             for family, r in zip(selected, rows)]
    dataset = {'schema_version': 1, 'dataset_id': VERSION, 'source': {'kind': 'independent',
        'reference': 'Public revision-pinned SQuAD1.1 TRAIN; supplied context; unused Fosnie article groups; model-training exposure and independence unreviewed.'}, 'cases': cases}
    validate_dataset(dataset)
    bank = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'evaluator_sha256': fingerprint(), 'cases': rows}
    validate(dataset, bank)
    manifest = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank),
        'evaluator_sha256': fingerprint(), 'source_manifest_sha256': digest(source_manifest), 'exposure_sha256': digest(exposure),
        'source_revision': REVISION, 'upstream_split': 'train', 'candidate_document_groups': 20, 'test_cases': 20,
        'prior_case_overlap': 0, 'prior_document_group_overlap': 0, 'published_span_offsets_verified': True,
        'model_outputs_used_for_selection': False, 'family_independence_reviewed': False, 'tier_labels_reviewed': False,
        'workflow_intent': 'extract a published answer span from provided text; not an independent minimum-tier label',
        'model_training_exposure_unknown': True, 'retrieval_exercised': False, 'quality_evidence': False, 'p12_eligible': False}
    return dataset, bank, manifest


def freeze(source_directory, pilot_root, output):
    source_directory = Path(source_directory); manifest = load(source_directory / 'manifest.json')
    document, raw = source(source_directory / 'train-v1.1.json', manifest); exposure = exposures(pilot_root)
    values = build(document, manifest, exposure); output = Path(output); output.mkdir(exist_ok=False)
    for name, value in zip(('dataset', 'answer-bank', 'manifest'), values): write_new(output / (name + '.json'), value)
    write_new(output / 'exposure.json', exposure); write_new(output / 'source-manifest.json', manifest)
    with (output / 'upstream-train-v1.1.json').open('xb') as stream: stream.write(raw)
    with (output / 'ATTRIBUTION.txt').open('xb') as stream: stream.write((source_directory / 'ATTRIBUTION.txt').read_bytes())
    return values


def corpus(directory):
    directory = Path(directory); manifest = load(directory / 'source-manifest.json'); exposure = load(directory / 'exposure.json')
    document, _ = source(directory / 'upstream-train-v1.1.json', manifest)
    expected = build(document, manifest, exposure)
    actual = tuple(load(directory / (name + '.json')) for name in ('dataset', 'answer-bank', 'manifest'))
    if actual != expected: raise ValueError('frozen span corpus or evaluator changed')
    return actual
