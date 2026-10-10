from copy import deepcopy
import json
import unittest

from judge_review import digest
from pilot_corpus import arithmetic,convert,VERSION
from unified_pilot import blueprint,choose_baseline,summarise,EXECUTORS


class CorpusTest(unittest.TestCase):
    def fixture(self):
        gsm=[{'question':'Problem '+chr(65+i),'answer':f'Step <<{i+1}+{i+2}={2*i+3}>>.\n#### {2*i+3}'} for i in range(5)]
        # Distinct calculation skeletons, not separate IDs of an identical template.
        gsm[1]['answer']='<<2*3=6>>\n#### 6';gsm[2]['answer']='<<9-3=6>>\n#### 6'
        squad={'data':[{'title':'Article '+chr(65+i),'paragraphs':[{'context':'Answer '+chr(65+i),
                 'qas':[{'id':'q'+str(i),'question':'Which answer '+chr(65+i)+'?','answers':[{'text':'Answer '+chr(65+i),'answer_start':0}]}]}]} for i in range(3)]}
        mera=[{'inputs':{'question':'Вопрос '+chr(65+i),'option_a':'один','option_b':'два','option_c':'три','option_d':'четыре'},
               'outputs':'A','meta':{'id':i}} for i in range(3)]
        docs={('gsm8k','test.jsonl'):'\n'.join(json.dumps(r) for r in gsm).encode(),
              ('squad','dev-v1.1.json'):json.dumps(squad).encode(),('mera','train.jsonl'):'\n'.join(json.dumps(r) for r in mera).encode()}
        sources={k:{'url':'https://source.example/'+k[0],'sha256':'a'*64} for k in docs}
        return docs,sources,{'schema_version':1,'sources':[]}

    def test_source_groups_stay_disjoint_and_wrappers_do_not_include_targets(self):
        docs,sources,manifest=self.fixture();result=convert(docs,sources,manifest,1,2)
        dataset,bank,summary,provenance=result
        self.assertEqual(len(dataset['cases']),9)
        cal={c['family_id'] for c in dataset['cases'] if c['split']=='calibration'}
        test={c['family_id'] for c in dataset['cases'] if c['split']=='test'}
        self.assertFalse(cal&test);self.assertFalse(summary['family_independence_reviewed']);self.assertFalse(summary['p12_eligible'])
        self.assertTrue(all(r['validator']=='strict_trimmed_one_of_v1' for r in bank['cases']))
        self.assertEqual(result,convert(docs,sources,manifest,1,2))
        self.assertEqual(len([r for r in provenance if r['source']=='gsm8k']),3)

    def test_published_calculation_error_and_invalid_span_cannot_be_verified(self):
        docs,sources,manifest=self.fixture()
        docs[('gsm8k','test.jsonl')]=b'{"question":"bad","answer":"<<1+1=3>>\\n#### 3"}'
        with self.assertRaises(ValueError):convert(docs,sources,manifest,1,2)

    def test_arithmetic_has_no_code_execution_and_bounded_rational_values(self):
        self.assertEqual(arithmetic('10/4+0.5'),3)
        for expression in ('__import__("os")','2**999999','a.b','999999999999999','1/0'):
            with self.subTest(expression=expression),self.assertRaises((ValueError,ZeroDivisionError)):arithmetic(expression)


class PilotTest(unittest.TestCase):
    def setUp(self):
        self.dataset={'schema_version':1,'dataset_id':VERSION,'source':{'kind':'synthetic','reference':'contract fixture'},'cases':[
            {'id':str(i),'family_id':'family'+str(i),'split':'calibration' if i<15 else 'test','language':'en','conversation':'Question '+str(i)}
            for i in range(75)]}
        self.bank={'dataset_sha256':digest(self.dataset)};self.manifest={'version':'synthetic'}
        self.records=[{'strategy':name,'case_id':str(i),'verdict':'pass' if name=='gemini-lite' else 'fail'} for i in range(15) for name in EXECUTORS]

    def test_total_budget_covers_all_phases_and_judge_calls(self):
        proposal=blueprint(self.dataset,self.bank,self.manifest)
        self.assertEqual((proposal['maximum_primary_calls'],proposal['maximum_judge_calls'],proposal['maximum_generation_calls']),(285,120,405))
        self.assertEqual(len(proposal['test_case_ids']),60)
        self.assertFalse(proposal['quality_evidence']);self.assertFalse(proposal['threshold_fitted'])

    def test_baseline_sees_only_complete_calibration_and_never_test_outcomes(self):
        self.assertEqual(choose_baseline(self.dataset,self.records)['selected'],'gemini-lite')
        foreign=self.records+[{'strategy':'openai','case_id':'16','verdict':'pass'}]
        with self.assertRaises(ValueError):choose_baseline(self.dataset,foreign)
        with self.assertRaises(ValueError):choose_baseline(self.dataset,self.records[:-1])
        with self.assertRaises(ValueError):choose_baseline(self.dataset,self.records+[self.records[0]])

    def test_missing_or_unknown_outputs_do_not_pass_and_duplicate_results_fail(self):
        report=summarise(self.dataset,self.records)
        for group in report['groups']:
            if group['split']=='test':self.assertEqual((group['unknown'],group['fully_passed_source_groups']),(60,0))
        self.assertFalse(report['judge_accuracy_evaluated']);self.assertFalse(report['p12_eligible'])
        with self.assertRaises(ValueError):summarise(self.dataset,self.records+[self.records[0]])


if __name__=='__main__':unittest.main()
