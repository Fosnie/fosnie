"""Calibrate a single baseline, then compare frozen Rules/Classifier/Hybrid on public text."""
from collections import Counter
import json
from pathlib import Path
import time

from audited_run import append_reserved, payload, preflight, score, snapshot
from judge_review import digest, write_new
from pilot_corpus import VERSION

EXECUTORS = ('qwen4b', 'openai', 'gemini-lite')


def blueprint(dataset, bank, manifest):
    if dataset.get('dataset_id') != VERSION or bank.get('dataset_sha256') != digest(dataset):raise ValueError('frozen public pilot required')
    calibration=[c for c in dataset['cases'] if c['split']=='calibration'];tests=[c for c in dataset['cases'] if c['split']=='test']
    if len(calibration)!=15 or len(tests)!=60:raise ValueError('fixed15calibration/60test cohort required')
    return {'schema_version':1,'allocation_id':'unified-auto-v15','approval_status':'pending',
            'dataset_sha256':digest(dataset),'answer_bank_sha256':digest(bank),'manifest_sha256':digest(manifest),
            'profiles':{'qwen4b':{'model':'huihui_ai/qwen3.5-abliterated:4b-Claude','adapter':'openai_chat','locality':'local','effort':'none','tier_hypothesis':'economy'},
                        'openai':{'model':'gpt-5.4-mini','adapter':'openai_responses','locality':'cloud','effort':'high','tier_hypothesis':'advanced'},
                        'gemini-lite':{'model':'gemini-3.1-flash-lite','adapter':'gemini','locality':'cloud','effort':'high','tier_hypothesis':'balanced'}},
            'judge':{'provider_name':'gemini-lite','effort':'low','native_schema':True,'output_tokens':1024,'min_confidence':0.75},
            'calibration_case_ids':[c['id'] for c in calibration],'test_case_ids':[c['id'] for c in tests],
            'calibration_baselines':list(EXECUTORS),'test_strategies':['best_single','rules','classifier','hybrid'],
            'baseline_selection':'calibration whole-family pass, then case pass, then stable configuration ID; never test results',
            'maximum_primary_calls':285,'maximum_judge_calls':120,'maximum_generation_calls':405,'maximum_cloud_calls':390,
            'maximum_cloud_calls_per_model':{'openai':240,'gemini-lite':375},
            'maximum_reserved_cloud_tokens':2000000,'maximum_reserved_cloud_tokens_per_model':1500000,
            'maximum_primary_output_tokens':2048,'maximum_request_cost_microusd':50000,
            'maximum_reserved_cost_microusd':14250000,'maximum_core_duration_ms':30000,'run_duration_seconds':7200,
            'gemini_spacing_seconds':10,'retry':False,'profile_quality_measured':False,'threshold_fitted':False,
            'quality_evidence':False,'p12_eligible':False,'family_independence_reviewed':False,
            'coverage':{'text_quality':['provided_context_en','reasoning_en','published_multiple_choice_ru'],
                        'retrieval_exercised':False,'web_exercised':False,'code_generation_exercised':False,'multi_turn_exercised':False,
                        'workflow_contracts':'separate existing Core/Enterprise fixtures, synthetic models; never text quality claims'}}


def choose_baseline(dataset, records):
    cases=[c for c in dataset['cases'] if c['split']=='calibration'];ids={c['id'] for c in cases}
    by_key={}
    for row in records:
        if row['case_id'] not in ids or row['strategy'] not in EXECUTORS:raise ValueError('baseline selection can only see calibration manual records')
        key=(row['strategy'],row['case_id'])
        if key in by_key:raise ValueError('duplicate calibration attempt')
        by_key[key]=row
    ranked=[]
    for name in EXECUTORS:
        families={c['family_id'] for c in cases};whole=sum(all(by_key.get((name,c['id']),{}).get('verdict')=='pass'
                    for c in cases if c['family_id']==f) for f in families)
        passed=sum(by_key.get((name,c['id']),{}).get('verdict')=='pass' for c in cases)
        ranked.append({'configuration_id':name,'passed_families':whole,'passed_cases':passed,'missing':sum((name,c['id']) not in by_key for c in cases)})
    if any(row['missing'] for row in ranked):raise ValueError('all calibration attempts must be recorded, including unknowns')
    chosen=min(ranked,key=lambda r:(-r['passed_families'],-r['passed_cases'],r['configuration_id']))
    return {'selected':chosen['configuration_id'],'ranking':ranked,'uses_test_outcomes':False,'quality_evidence':False}


def summarise(dataset, records):
    cases={c['id']:c for c in dataset['cases']};indexed={};groups=[]
    for row in records:
        if row['case_id'] not in cases or row.get('verdict') not in ('pass','fail','unknown'):raise ValueError('foreign result')
        key=(row['strategy'],row['case_id'])
        if key in indexed:raise ValueError('duplicate result; no best retry selection')
        indexed[key]=row
    for split,strategies in (('calibration',EXECUTORS),('test',('best_single','rules','classifier','hybrid'))):
        selected=[c for c in cases.values() if c['split']==split]
        for strategy in strategies:
            rows=[indexed.get((strategy,c['id']),{}) for c in selected];counts=Counter(r.get('verdict','unknown') for r in rows)
            families={c['family_id'] for c in selected}
            passed=sum(all(indexed.get((strategy,c['id']),{}).get('verdict')=='pass' for c in selected if c['family_id']==f) for f in families)
            groups.append({'split':split,'strategy':strategy,'cases':len(selected),'pass':counts['pass'],'fail':counts['fail'],'unknown':counts['unknown'],
                           'candidate_source_groups':len(families),'fully_passed_source_groups':passed,
                           'judge_calls':sum(r.get('classifier_ledger',{}).get('calls',0) for r in rows),
                           'observed_completed_model_calls':sum(r.get('usage',{}).get('calls',0) for r in rows),
                           'unknown_cost_rows':sum(r.get('usage',{}).get('unknown_cost_calls',0)>0 or r.get('contract_status')!='completed' for r in rows),
                           'actual_cost_microusd':None,'requirements_label_accuracy':None})
    paired=[]
    for strategy in ('rules','classifier','hybrid'):
        rows=[(indexed.get(('best_single',c['id']),{}),indexed.get((strategy,c['id']),{})) for c in cases.values() if c['split']=='test']
        paired.append({'strategy':strategy,'both_pass':sum(a.get('verdict')==b.get('verdict')=='pass' for a,b in rows),
                       'baseline_only':sum(a.get('verdict')=='pass' and b.get('verdict')!='pass' for a,b in rows),
                       'auto_only':sum(a.get('verdict')!='pass' and b.get('verdict')=='pass' for a,b in rows),
                       'neither_pass':sum(a.get('verdict')!='pass' and b.get('verdict')!='pass' for a,b in rows)})
    return {'schema_version':1,'dataset_sha256':digest(dataset),'groups':groups,'paired_test':paired,
            'quality_evidence':False,'family_independence_reviewed':False,'judge_accuracy_evaluated':False,
            'actual_savings_microusd':None,'policy_updated':False,'p12_eligible':False}


def execute(dataset, bank, config, proposed, output, client, native_guard, clock=time.monotonic):
    if proposed!=blueprint(dataset,bank,config['source_manifest']):raise ValueError('matching whole-pilot proposal required')
    output=Path(output);output.mkdir(exist_ok=False);write_new(output/'allocation.json',proposed)
    report={'schema_version':1,'dataset_sha256':digest(dataset),'allocation_sha256':digest(proposed),'records':[],
            'complete':False,'stop_reason':None,'reserved_model_calls_upper':0,'reserved_cost_upper_microusd':0,
            'quality_evidence':False,'source':native_guard.source,'policy_updated':False,'baseline_selection':None}
    profiles=config['profiles'];policies=config['policies'];runtime=config['runtime']
    case_by_id={c['id']:c for c in dataset['cases']};answers={c['case_id']:c['acceptable_answers'] for c in bank['cases']}
    start=clock()
    def attempt(strategy,case,baseline=None):
        remaining=proposed['run_duration_seconds']-(clock()-start)
        if remaining<35:raise TimeoutError('pilot deadline')
        preflight(client,runtime,min(5,remaining))
        selection=config['selections'][baseline if strategy=='best_single' else strategy]
        request=payload(case,selection,runtime)
        # These caller hints come from the supplied-document workflow, never answer/tier labels.
        request['requirements']['task']='extraction' if case['id'].startswith('squad/') else 'unknown'
        maximum=2 if strategy in ('classifier','hybrid') else 1
        reservation={'strategy':strategy,'case_id':case['id'],'evaluation_id':request['evaluation_id'],
                     'request_sha256':digest(request),'maximum_model_calls':maximum,'reserved_cost_upper_microusd':request['max_cost_microusd']}
        append_reserved(output/'core-reservations.jsonl',reservation)
        report['reserved_model_calls_upper']+=maximum;report['reserved_cost_upper_microusd']+=request['max_cost_microusd']
        row={'strategy':strategy,'case_id':case['id'],'evaluation_id':request['evaluation_id'],'verdict':'unknown','contract_status':'failed','target_matches':False}
        began=clock();native_guard.expect(strategy,case,baseline)
        try:
            value=client.request('/api/routing/evaluate',request,min(35,remaining))
            row.update(score(value,request,selection,runtime,answers[case['id']][0]))
            if row['contract_status']=='completed':row['verdict']='pass' if value['content'].strip() in answers[case['id']] else 'fail'
        except (OSError,TimeoutError):row['failure']='transport_or_http_refusal'
        except (ValueError,KeyError,TypeError,StopIteration):row['failure']='response_contract_mismatch';report['stop_reason']='response_contract_mismatch'
        finally:native_guard.clear()
        row['elapsed_ms']=round((clock()-began)*1000);row['source']=native_guard.source
        report['records'].append(row);snapshot(output/'report.partial.json',report)
        if report['stop_reason']:raise ValueError('contract mismatch')
    try:
        for case_id in proposed['calibration_case_ids']:
            for name in EXECUTORS:attempt(name,case_by_id[case_id])
        selection=choose_baseline(dataset,report['records']);report['baseline_selection']=selection
        write_new(output/'frozen-baseline.json',selection)
        for case_id in proposed['test_case_ids']:
            for strategy in proposed['test_strategies']:attempt(strategy,case_by_id[case_id],selection['selected'])
        report['complete']=len(report['records'])==285
    except (ValueError,KeyError,TimeoutError,OSError):
        report['stop_reason']=report['stop_reason'] or 'pilot_preflight_deadline_or_persistence_failed'
    finally:
        report['comparison']=summarise(dataset,report['records']);write_new(output/'report.json',report)
        snapshot(output/'report.partial.json',report)
    return report
