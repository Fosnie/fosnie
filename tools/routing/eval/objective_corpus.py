"""Freeze unused BBH instances with recomputed published final values; no network."""
import hashlib
import json
from pathlib import Path

from cohort_report import public_cohort
from judge_review import digest, encoded, load as read_small, prompt_digest, write_new
from semantic_review import exact
from objective_answers import DIRECTIVE, TASKS, fingerprint, validate
from reference_audit import solve, Unsupported
from public_benchmark import INSTRUCTION

VERSION = 'fosnie-formal-public-v40'


def exposures(pilot_root):
    root = Path(pilot_root).resolve(); ids, sources, prompts = set(), [], set()
    for path in sorted(root.rglob('*.json')):
        if path.name not in ('dataset.json', 'prompts.json'): continue
        value = read_small(path)
        if not isinstance(value, dict) or not isinstance(value.get('cases'), list): continue
        local_ids = []
        for case in value['cases']:
            identity = case.get('id') if isinstance(case, dict) else None
            if isinstance(identity, str) and identity.startswith('bbh/'):
                ids.add(identity); local_ids.append(identity)
                conversation = case.get('conversation')
                if isinstance(conversation, str):
                    prompts.add(prompt_digest(conversation.removesuffix(INSTRUCTION)))
        if local_ids: sources.append({'path': str(path.relative_to(root)), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
    return {'schema_version': 1, 'case_ids': sorted(ids), 'original_prompt_sha256': sorted(prompts), 'files': sources}


def build(documents, provenance, exposure):
    cases, answers = [], []
    excluded = set(exposure['case_ids']); seen = set(exposure['original_prompt_sha256'])
    source_by_task = {row['task']: row for row in provenance['sources']}
    for task in TASKS:
        raw = documents[task]
        if hashlib.sha256(raw).hexdigest() != source_by_task[task]['sha256']:
            raise ValueError('pinned formal source hash mismatch')
        examples = json.loads(raw)['examples']; eligible = []
        for index, example in enumerate(examples):
            identity = f'bbh/{task}/{index}'; question = example['input']
            if identity in excluded or prompt_digest(question) in seen: continue
            try:
                solved, value, kind, proof = solve({'family_id': 'bbh/' + task, 'conversation': question + INSTRUCTION})
            except Unsupported: continue
            if solved != example['target']: continue  # No label corrections or ambiguous grammar.
            eligible.append((digest([VERSION, task, index, example]), identity, index, question, example['target'], value, kind, proof))
        if len(eligible) < 4: raise ValueError('insufficient unused verified formal cases')
        for _, identity, index, question, source_target, target, kind, proof in sorted(eligible)[:4]:
            seen.add(prompt_digest(question))
            cases.append({'id': identity, 'family_id': 'bbh/' + task, 'split': 'test', 'language': 'en', 'conversation': question + DIRECTIVE})
            answers.append({'case_id': identity, 'source_task': task, 'source_index': index, 'original_question': question,
                'source_target': source_target, 'target': target, 'kind': kind, 'proof': proof,
                'original_input_sha256': hashlib.sha256(question.encode('utf-8')).hexdigest()})
    dataset = {'schema_version': 1, 'dataset_id': VERSION, 'source': {'kind': 'independent',
        'reference': 'Revision-pinned public BBH; unused instance IDs; task groups/training exposure unreviewed; final-format directive added.'}, 'cases': cases}
    bank = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'evaluator_sha256': fingerprint(), 'cases': answers}
    validate(dataset, bank)
    manifest = {'schema_version': 1, 'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank),
        'evaluator_sha256': fingerprint(), 'parent_provenance_sha256': digest(provenance), 'exposure_sha256': digest(exposure),
        'source_revision': provenance['revision'], 'source_files': [source_by_task[t] for t in TASKS],
        'source_task_groups': 5, 'test_instances': 20, 'previous_case_overlap': 0, 'previous_original_prompt_overlap': 0,
        'eligibility': 'bounded existing formal solver agrees with published target',
        'model_outputs_used_for_selection': False, 'family_independence_reviewed': False,
        'workload_tier_labels_reviewed': False, 'task_groups_previously_exposed': True,
        'training_exposure_unknown': True, 'prompt_directive_added': True, 'quality_evidence': False, 'p12_eligible': False}
    return dataset, bank, manifest


def freeze(parent, pilot_root, output):
    parent = Path(parent); output = Path(output)
    _, _, provenance = public_cohort(parent)
    exposure = exposures(pilot_root)
    documents = {t: (parent / 'upstream' / (t + '.json')).read_bytes() for t in TASKS}
    values = build(documents, provenance, exposure)
    output.mkdir(exist_ok=False)
    for name, value in zip(('dataset', 'answer-bank', 'manifest'), values): write_new(output / (name + '.json'), value)
    write_new(output / 'exposure.json', exposure); write_new(output / 'parent-provenance.json', provenance)
    source = output / 'upstream'; source.mkdir()
    for task, raw in documents.items():
        with (source / (task + '.json')).open('xb') as stream: stream.write(raw)
    with (output / 'LICENSE-BBH.txt').open('xb') as stream: stream.write((parent / 'LICENSE-BBH.txt').read_bytes())
    return values


def corpus(directory):
    directory = Path(directory); provenance = read_small(directory / 'parent-provenance.json'); exposure = read_small(directory / 'exposure.json')
    exact(exposure, ('schema_version', 'case_ids', 'original_prompt_sha256', 'files'), 'invalid exposure registry')
    documents = {}
    for task in TASKS:
        with (directory / 'upstream' / (task + '.json')).open('rb') as stream: raw = stream.read(2 * 1024 * 1024 + 1)
        if len(raw) > 2 * 1024 * 1024: raise ValueError('formal source bound')
        documents[task] = raw
    expected = build(documents, provenance, exposure)
    actual = tuple(read_small(directory / (name + '.json')) for name in ('dataset', 'answer-bank', 'manifest'))
    if actual != expected: raise ValueError('frozen formal corpus or evaluator changed')
    return actual
