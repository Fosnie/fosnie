from copy import deepcopy
import unittest
from judge_diagnostics import analyse

class DiagnosticsTest(unittest.TestCase):
    def setUp(self):
        self.case={'id':'proof','family_id':'proof-family','split':'test','tasks':['reasoning'],'minimum_tier':'advanced',
            'fresh_information':False,'tools':False,'vision':False,'structured_output':True,'uncertain':False}
        self.raw={**{k:v for k,v in self.case.items() if k not in {'id','family_id','split','tasks'}},'task':'reasoning','confidence':1.0}
        self.labels={'schema_version':1,'cases':[self.case]}
        self.observations={'schema_version':1,'models':['local','cloud'],'records':[{'model':'local','case_id':'proof','status':'completed','classification':self.raw}]}
    def test_confident_field_error_and_missing_response_do_not_become_passes(self):
        self.raw['fresh_information']=True
        result=analyse(self.labels,self.observations)
        local=next(g for g in result['groups'] if g['model']=='local');cloud=next(g for g in result['groups'] if g['model']=='cloud')
        self.assertEqual(local['mismatch'],1);self.assertEqual(local['field_errors']['fresh_information'],1)
        self.assertEqual(local['confidence_buckets']['[0.95,1]']['with_field_errors'],1);self.assertEqual(cloud['unknown'],1)
        self.assertFalse(result['threshold_fitted']);self.assertFalse(result['answer_quality_evaluated'])
    def test_threshold_changes_abstention_without_hiding_label_errors(self):
        self.raw.update(confidence=.8,minimum_tier='balanced')
        result=analyse(self.labels,self.observations,.9)
        local=next(g for g in result['groups'] if g['model']=='local')
        self.assertEqual(local['abstained'],1);self.assertEqual(local['field_errors']['tier_below_label'],1)
    def test_invalid_schema_duplicates_and_split_leakage_rejected(self):
        changed=deepcopy(self.observations);changed['records']*=2
        with self.assertRaises(ValueError):analyse(self.labels,changed)
        changed=deepcopy(self.labels);changed['cases'].append({**self.case,'id':'proof-2','split':'calibration'})
        with self.assertRaises(ValueError):analyse(changed,self.observations)
        for value in [-1,2,float('nan')]:
            with self.assertRaises(ValueError):analyse(self.labels,self.observations,value)
    def test_bad_classification_remains_unknown(self):
        self.raw['confidence']=float('nan')
        self.assertEqual(analyse(self.labels,self.observations)['records'][0]['outcome'],'unknown')

if __name__=='__main__':unittest.main()
