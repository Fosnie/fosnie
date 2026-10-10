"""Offline field-level diagnostics; never call models or update routing policy."""
from collections import Counter
import argparse
import hashlib
import json
import math
from pathlib import Path

TASKS={'unknown','current_fact','general','editing','extraction','coding','reasoning','research'}
TIERS={'economy':0,'balanced':1,'advanced':2}
FLAGS=('fresh_information','tools','vision','structured_output','uncertain')
KEYS={'task','minimum_tier','confidence',*FLAGS}

def valid_classification(value):
    return (isinstance(value,dict) and set(value)==KEYS and isinstance(value['task'],str) and value['task'] in TASKS
        and isinstance(value['minimum_tier'],str) and value['minimum_tier'] in TIERS and all(type(value[k]) is bool for k in FLAGS)
        and type(value['confidence']) in (int,float) and math.isfinite(value['confidence'])
        and 0<=value['confidence']<=1)

def analyse(labels,observations,threshold=0.75):
    if not math.isfinite(threshold) or not 0<=threshold<=1: raise ValueError('invalid confidence threshold')
    if labels.get('schema_version')!=1 or observations.get('schema_version')!=1: raise ValueError('unsupported schema')
    cases=labels.get('cases');models=observations.get('models');records=observations.get('records')
    if not isinstance(cases,list) or not 1<=len(cases)<=1000 or not isinstance(records,list) or len(records)>10000: raise ValueError('case/record bound')
    if not isinstance(models,list) or not 1<=len(models)<=32 or len(set(models))!=len(models) or any(not isinstance(m,str) or not m or len(m)>256 for m in models): raise ValueError('invalid model list')
    by_id={};family_splits={}
    for case in cases:
        if set(case)!={'id','family_id','split','tasks','minimum_tier',*FLAGS}: raise ValueError('invalid label fields')
        if not isinstance(case['id'],str) or not 1<=len(case['id'])<=128 or case['id'] in by_id: raise ValueError('duplicate/invalid case ID')
        if not isinstance(case['family_id'],str) or not 1<=len(case['family_id'])<=128 or case['split'] not in {'calibration','test'}: raise ValueError('invalid family/split')
        if case['family_id'] in family_splits and family_splits[case['family_id']]!=case['split']: raise ValueError('family leakage between splits')
        if not isinstance(case['tasks'],list) or not case['tasks'] or not set(case['tasks'])<=TASKS or case['minimum_tier'] not in TIERS or any(type(case[k]) is not bool for k in FLAGS): raise ValueError('invalid expected requirements')
        by_id[case['id']]=case;family_splits[case['family_id']]=case['split']
    indexed={}
    for record in records:
        if set(record)!={'case_id','model','status','classification'} or record['model'] not in models or record['case_id'] not in by_id or record['status'] not in {'completed','failed'}: raise ValueError('invalid observation')
        key=(record['model'],record['case_id'])
        if key in indexed: raise ValueError('duplicate observation; retries must be reported separately')
        indexed[key]=record
    rows=[];groups={}
    for model in models:
        for case in cases:
            record=indexed.get((model,case['id']));raw=record.get('classification') if record else None
            valid=record is not None and record['status']=='completed' and valid_classification(raw)
            errors=[]
            if valid:
                if raw['task'] not in case['tasks']:errors.append('task')
                if TIERS[raw['minimum_tier']]<TIERS[case['minimum_tier']]:errors.append('tier_below_label')
                if TIERS[raw['minimum_tier']]>TIERS[case['minimum_tier']]:errors.append('tier_above_label')
                errors += [field for field in FLAGS if raw[field]!=case[field]]
            abstained=valid and (raw['uncertain'] or raw['task']=='unknown' or raw['confidence']<threshold)
            outcome='unknown' if not valid else 'abstained' if abstained else 'mismatch' if errors else 'match'
            row={'model':model,'case_id':case['id'],'family_id':case['family_id'],'split':case['split'],'outcome':outcome,'field_errors':errors,
                'confidence':raw['confidence'] if valid else None}
            rows.append(row)
            group=groups.setdefault((model,case['split']),{'total':0,'match':0,'mismatch':0,'abstained':0,'unknown':0,'field_errors':Counter(),'confidence_buckets':{}})
            group['total']+=1;group[outcome]+=1
            if valid:
                group['field_errors'].update(errors)
                bucket='[0.95,1]' if raw['confidence']>=.95 else '[0.75,0.95)' if raw['confidence']>=.75 else '[0,0.75)'
                counts=group['confidence_buckets'].setdefault(bucket,{'completed':0,'with_field_errors':0})
                counts['completed']+=1;counts['with_field_errors']+=bool(errors)
    # Language variants/turns do not create independent samples. Every case
    # in a family must match; abstentions, field errors and missing reports
    # remain unsuccessful for this conservative classification metric.
    families={}
    for row in rows:
        key=(row['model'],row['split'],row['family_id'])
        families.setdefault(key,[]).append(row['outcome'])
    family_groups={}
    for (model,split,family),outcomes in families.items():
        group=family_groups.setdefault((model,split),{'families':0,'matching_families':0})
        group['families']+=1;group['matching_families']+=all(outcome=='match' for outcome in outcomes)
    for group in family_groups.values():
        n=group['families'];rate=group['matching_families']/n;z=1.96
        group['wilson95_lower_bound']=(rate+z*z/(2*n)-z*math.sqrt(rate*(1-rate)/n+z*z/(4*n*n)))/(1+z*z/n)
    digest=hashlib.sha256(json.dumps(labels,sort_keys=True,separators=(',',':')).encode()).hexdigest()
    return {'schema_version':1,'labels_sha256':digest,'min_confidence':threshold,'threshold_fitted':False,
        'confidence_is_calibrated_probability':False,'quality_evidence':False,'policy_updated':False,'model_calls':0,'answer_quality_evaluated':False,
        'family_groups':[{'model':model,'split':split,**group} for (model,split),group in sorted(family_groups.items())],
        'groups':[{'model':model,'split':split,**group} for (model,split),group in sorted(groups.items())], 'records':rows}

def load(path):
    raw=Path(path).read_bytes()
    if len(raw)>4*1024*1024:raise ValueError('input exceeds 4 MiB')
    return json.loads(raw,parse_constant=lambda _:(_ for _ in ()).throw(ValueError('non-finite JSON value')))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--labels',required=True);parser.add_argument('--observations',required=True)
    parser.add_argument('--output',required=True);parser.add_argument('--min-confidence',type=float,default=.75)
    args=parser.parse_args();result=analyse(load(args.labels),load(args.observations),args.min_confidence)
    Path(args.output).write_text(json.dumps(result,indent=2,ensure_ascii=False)+'\n',encoding='utf-8')
    print(json.dumps({key:result[key] for key in ('model_calls','policy_updated','threshold_fitted','groups')}))

if __name__=='__main__':main()
