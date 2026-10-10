"""Import a small, revision-pinned BBH cohort. Download is explicit; no inference."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import re
import urllib.request

from judge_review import digest, encoded, freeze, load, prepare_files, write_new

REPOSITORY = 'suzgunmirac/BIG-Bench-Hard'
CALIBRATION = {'boolean_expressions', 'date_understanding', 'word_sorting'}
GROUPED = {'logical_deduction_three_objects':'logical_deduction', 'tracking_shuffled_objects_three_objects':'tracking_shuffled_objects'}
TASKS = (
    'boolean_expressions', 'causal_judgement', 'date_understanding', 'disambiguation_qa', 'dyck_languages', 'formal_fallacies',
    'geometric_shapes', 'hyperbaton', 'logical_deduction_three_objects', 'movie_recommendation', 'multistep_arithmetic_two',
    'navigate', 'object_counting', 'penguins_in_a_table', 'reasoning_about_colored_objects', 'ruin_names',
    'salient_translation_error_detection', 'snarks', 'sports_understanding', 'temporal_sequences',
    'tracking_shuffled_objects_three_objects', 'web_of_lies', 'word_sorting',
)
INSTRUCTION = '\n\nReturn only the final answer in the format requested above, with no explanation.'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        raise ValueError('benchmark redirects are forbidden')


def fetch(url):
    if not (url.startswith('https://api.github.com/repos/'+REPOSITORY+'/') or url.startswith('https://raw.githubusercontent.com/'+REPOSITORY+'/')):
        raise ValueError('unapproved dataset endpoint')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(urllib.request.Request(url,headers={'User-Agent':'Fosnie-public-benchmark-intake'}),timeout=25) as response:
        data = response.read(2*1024*1024+1)
    if len(data)>2*1024*1024:
        raise ValueError('dataset download exceeds 2 MiB')
    return data


def convert(revision, documents):
    if not re.fullmatch('[a-f0-9]{40}',revision) or set(documents)!=set(TASKS):
        raise ValueError('invalid benchmark revision or task coverage')
    cases, targets, sources = [], [], []
    for task in TASKS:
        raw = documents[task]
        value = json.loads(raw)
        if not isinstance(value,dict) or not isinstance(value.get('examples'),list) or not 2<=len(value['examples'])<=10000 or not isinstance(value.get('canary'),str):
            raise ValueError('invalid benchmark task')
        for example in value['examples']:
            if set(example)!={'input','target'} or not isinstance(example['input'],str) or not example['input'].strip() or not isinstance(example['target'],str) or not example['target'].strip():
                raise ValueError('unsupported benchmark example')
        family = GROUPED.get(task,task)
        split = 'calibration' if family in CALIBRATION else 'test'
        # Fixed canonical ordering before any model output or correctness test.
        indexed = sorted(enumerate(value['examples']),key=lambda pair:digest(pair[1]))[:2]
        url = f'https://raw.githubusercontent.com/{REPOSITORY}/{revision}/bbh/{task}.json'
        source_hash = hashlib.sha256(raw).hexdigest()
        sources.append({'task':task,'family_id':'bbh/'+family,'revision':revision,'url':url,'sha256':source_hash,
                        'source_examples':len(value['examples']),'canary':value['canary']})
        for index, example in indexed:
            identity=f'bbh/{task}/{index}'
            prompt=example['input']+INSTRUCTION
            cases.append({'id':identity,'family_id':'bbh/'+family,'split':split,'language':'en','conversation':prompt})
            targets.append({'case_id':identity,'family_id':'bbh/'+family,'split':split,'target':example['target'],
                            'original_index':index,'source_url':url,'source_sha256':source_hash,
                            'original_input_sha256':hashlib.sha256(example['input'].encode('utf-8')).hexdigest()})
    dataset={'schema_version':1,'dataset_id':'public-bbh-v1','source':{'kind':'independent',
             'reference':f'https://github.com/{REPOSITORY}/tree/{revision}; external benchmark, not Fosnie tier labels'},'cases':cases}
    freeze(dataset)  # all local invariants/exposure checks before writing
    answer_bank={'schema_version':1,'dataset_sha256':digest(dataset),'validator':'exact_trimmed_text_v1','cases':targets}
    provenance={'schema_version':1,'repository':REPOSITORY,'revision':revision,'sampling':'first two canonical SHA256 examples per task',
                'calibration_families':sorted(CALIBRATION),'test_families':20,'task_variant_groups':GROUPED,
                'dataset_sha256':digest(dataset),'answer_bank_sha256':digest(answer_bank),'sources':sources,
                'minimum_tier_labels_reviewed':False,'public_benchmark_may_be_in_model_training':True,
                'coverage':'English static reasoning/language tasks; no live web, Russian, images or enterprise workflow coverage'}
    return dataset, answer_bank, provenance


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--download',action='store_true',help='Explicitly fetch the allowlisted public JSON files')
    parser.add_argument('--output',required=True)
    parser.add_argument('--revision',help='Use a known 40-character upstream commit; otherwise freeze main once')
    args=parser.parse_args()
    if not args.download:
        print(json.dumps({'status':'not_run','model_calls':0,'network_calls':0}));return
    destination=Path(args.output)
    if destination.exists():
        raise ValueError('output already exists')
    revision=args.revision or json.loads(fetch('https://api.github.com/repos/'+REPOSITORY+'/commits/main'))['sha']
    if not re.fullmatch('[a-f0-9]{40}',revision):
        raise ValueError('invalid revision')
    prefix=f'https://raw.githubusercontent.com/{REPOSITORY}/{revision}/'
    licence=fetch(prefix+'LICENSE')
    if not licence.startswith(b'MIT License'):
        raise ValueError('unexpected licence')
    with ThreadPoolExecutor(max_workers=4) as pool:
        documents=dict(zip(TASKS,pool.map(lambda name:fetch(prefix+'bbh/'+name+'.json'),TASKS)))
    dataset, answers, provenance=convert(revision,documents)
    destination.mkdir(parents=False,exist_ok=False)
    write_new(destination/'prompts.json',dataset);write_new(destination/'answer-bank.json',answers);write_new(destination/'provenance.json',provenance)
    with (destination/'LICENSE-BBH.txt').open('xb') as output:output.write(licence)
    raw=destination/'upstream';raw.mkdir()
    for task,data in documents.items():
        with (raw/(task+'.json')).open('xb') as output:output.write(data)
    prepare_files(destination/'prompts.json',destination/'blind-review')
    print(json.dumps({'status':'prepared','cases':len(dataset['cases']),'families':23,'test_families':20,'calibration_families':3,
                      'revision':revision,'network_calls':len(TASKS)+1+int(args.revision is None),'model_calls':0,'quality_evidence':False}))


if __name__=='__main__':main()
