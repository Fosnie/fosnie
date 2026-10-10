"""Offline, family-aware public answer comparison. No routing or model calls."""
import argparse
from collections import Counter
import hashlib
import math
from pathlib import Path

from judge_review import digest, load, validate_dataset, write_new
from public_benchmark import TASKS, convert


def public_cohort(directory):
    directory=Path(directory)
    dataset=load(directory/'prompts.json');answers=load(directory/'answer-bank.json');provenance=load(directory/'provenance.json')
    validate_dataset(dataset)
    if provenance.get('dataset_sha256')!=digest(dataset) or provenance.get('answer_bank_sha256')!=digest(answers):
        raise ValueError('public cohort binding mismatch')
    documents={}
    for task in TASKS:
        path=(directory/'upstream'/f'{task}.json').resolve()
        if not path.is_relative_to((directory/'upstream').resolve()):
            raise ValueError('source path outside cohort')
        with path.open('rb') as stream:raw=stream.read(2*1024*1024+1)
        if len(raw)>2*1024*1024:raise ValueError('source exceeds bound')
        documents[task]=raw
    rebuilt=convert(provenance['revision'],documents)
    if rebuilt!=(dataset,answers,provenance):
        raise ValueError('cohort differs from frozen upstream sample')
    return dataset,answers,provenance


def lower_bound(passed,total):
    if not total:return None
    n=float(total);p=passed/n;z=1.96
    return max(0.0,min(1.0,(p+z*z/(2*n)-z*math.sqrt(p*(1-p)/n+z*z/(4*n*n)))/(1+z*z/n)))


def summarise(dataset,observations):
    validate_dataset(dataset)
    if observations.get('schema_version')!=1 or observations.get('dataset_sha256')!=digest(dataset):
        raise ValueError('observation dataset mismatch')
    configs=observations.get('configurations');records=observations.get('records')
    if not isinstance(configs,list) or not 1<=len(configs)<=32 or not isinstance(records,list) or len(records)>32000:
        raise ValueError('configuration/record bounds')
    by_model={}
    for config in configs:
        if (not isinstance(config,dict) or set(config)!= {'id','model','effort','profile_sha256'} or not isinstance(config['id'],str)
            or not config['id'] or config['id'] in by_model or not isinstance(config['model'],str) or not config['model']
            or config['effort'] is not None and not isinstance(config['effort'],str)
            or not isinstance(config['profile_sha256'],str) or len(config['profile_sha256'])!=64
            or any(c not in '0123456789abcdef' for c in config['profile_sha256'])):
            raise ValueError('invalid fixed model configuration')
        by_model[config['id']]=config
    cases={c['id']:c for c in dataset['cases']};indexed={}
    for row in records:
        if not isinstance(row,dict) or row.get('configuration_id') not in by_model or row.get('case_id') not in cases:
            raise ValueError('foreign observation')
        key=(row['configuration_id'],row['case_id'])
        if key in indexed:raise ValueError('duplicate observation; do not select a successful retry')
        config=by_model[row['configuration_id']]
        if row.get('profile_sha256')!=config['profile_sha256'] or row.get('effort')!=config['effort']:
            raise ValueError('mixed profile/effort configurations')
        if row.get('verdict') not in ('pass','fail','unknown') or row.get('contract_status') not in ('completed','failed'):
            raise ValueError('invalid observation result')
        if row['verdict']!='unknown' and (row['contract_status']!='completed' or row.get('finish_reason')!='stop' or row.get('target_matches') is not True):
            raise ValueError('truncated/unattested answer cannot be scored')
        indexed[key]=row
    groups=[];families=[]
    for identity in by_model:
        for split in ('calibration','test','all'):
            cohort=[c for c in cases.values() if split=='all' or c['split']==split]
            counts=Counter(indexed.get((identity,c['id']),{}).get('verdict','unknown') for c in cohort)
            family_ids=sorted({c['family_id'] for c in cohort});passed=complete=0
            for family in family_ids:
                members=[c for c in cohort if c['family_id']==family]
                rows=[indexed.get((identity,c['id'])) for c in members]
                observed=all(row is not None for row in rows)
                success=all(row is not None and row['verdict']=='pass' for row in rows)
                complete+=observed;passed+=success
                if split!='all':families.append({'configuration_id':identity,'split':split,'family_id':family,
                    'case_count':len(members),'complete_observations':observed,'passed':success})
            groups.append({'configuration_id':identity,'split':split,'cases':len(cohort),
                'attempted_records':sum((identity,c['id']) in indexed for c in cohort),
                'pass':counts['pass'],'fail':counts['fail'],'unknown':counts['unknown'],
                'families':len(family_ids),'complete_families':complete,'passed_families':passed,
                'wilson95_family_lower_bound':lower_bound(passed,len(family_ids))})
    # This baseline chooses only on calibration; test answers never choose it.
    calibration=[g for g in groups if g['split']=='calibration']
    chosen=min(calibration,key=lambda g:(-g['pass'],g['configuration_id']))['configuration_id'] if calibration and calibration[0]['cases'] else None
    tests=[c for c in cases.values() if c['split']=='test']
    oracle={c['id']:any(indexed.get((identity,c['id']),{}).get('verdict')=='pass' for identity in by_model) for c in tests}
    oracle_families=sum(all(oracle[c['id']] for c in tests if c['family_id']==family) for family in {c['family_id'] for c in tests})
    paired=[]
    names=list(by_model)
    for i,left in enumerate(names):
        for right in names[i+1:]:
            counts=Counter()
            for case in tests:
                a=indexed.get((left,case['id']),{}).get('verdict')=='pass';b=indexed.get((right,case['id']),{}).get('verdict')=='pass'
                counts['both_pass' if a and b else 'left_only' if a else 'right_only' if b else 'neither_pass']+=1
            paired.append({'left':left,'right':right,'split':'test','cases':len(tests),**counts})
    return {'schema_version':1,'dataset_sha256':digest(dataset),'configuration_sha256':digest(configs),
        'quality_evidence':False,'label_accuracy_evaluated':False,'policy_updated':False,'model_calls':0,
        'groups':groups,'families':families,'paired_test':paired,
        'best_single_calibration':{'configuration_id':chosen,'selection':'calibration reference passes, then stable ID tie-break'},
        'post_answer_oracle':{'split':'test','cases':len(tests),'passed_cases':sum(oracle.values()),'passed_families':oracle_families,
                            'deployable_router':False,'uses_reference_answers_after_inference':True},
        'statistical_claim':False,'actual_cost_microusd':None}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cohort',required=True);parser.add_argument('--observations',required=True);parser.add_argument('--output',required=True)
    parser.add_argument('--parent',help='Required frozen public parent when comparing an audited derived cohort')
    args=parser.parse_args()
    if args.parent:
        from reference_audit import check
        dataset,_,_,_=check(args.cohort,args.parent)
    else:
        dataset,_,_=public_cohort(args.cohort)
    report=summarise(dataset,load(args.observations));write_new(args.output,report)
    print('Created offline family comparison; no policy, promotion or inference.')


if __name__=='__main__':main()
