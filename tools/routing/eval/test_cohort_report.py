from copy import deepcopy
import unittest

from cohort_report import summarise
from judge_review import digest


class CohortReportTest(unittest.TestCase):
    def setUp(self):
        self.dataset={'schema_version':1,'dataset_id':'test-cohort','source':{'kind':'synthetic','reference':'contract-only fixture'},
            'cases':[{'id':name,'family_id':family,'split':split,'language':'en','conversation':name+' independent question'}
                     for name,family,split in [('cal-a','calibration','calibration'),('test-a','family','test'),('test-b','family','test')]]}
        self.configs=[{'id':name,'model':name,'effort':None,'profile_sha256':name*64} for name in ('a','b')]
        self.rows=[]
        for model in ('a','b'):
            for name in ('cal-a','test-a','test-b'):
                passed=(model=='a' and name!='test-b') or (model=='b' and name!='cal-a')
                self.rows.append({'configuration_id':model,'case_id':name,'effort':None,'profile_sha256':model*64,
                                  'verdict':'pass' if passed else 'fail','contract_status':'completed','finish_reason':'stop','target_matches':True})
        self.observations={'schema_version':1,'dataset_sha256':digest(self.dataset),'configurations':self.configs,'records':self.rows}

    def test_pair_and_calibration_baseline_do_not_choose_on_test_or_count_variants_as_families(self):
        report=summarise(self.dataset,self.observations)
        self.assertEqual(report['best_single_calibration']['configuration_id'],'a')
        test=next(g for g in report['groups'] if g['configuration_id']=='a' and g['split']=='test')
        self.assertEqual((test['pass'],test['families'],test['passed_families']),(1,1,0))
        self.assertEqual(report['paired_test'][0]['right_only'],1)
        self.assertEqual(report['post_answer_oracle']['passed_cases'],2)
        self.assertFalse(report['post_answer_oracle']['deployable_router'])
        self.assertFalse(report['quality_evidence'])
        self.assertIsNone(report['actual_cost_microusd'])

    def test_missing_variant_counts_unknown_and_prevents_family_pass(self):
        changed=deepcopy(self.observations);changed['records']=[r for r in changed['records'] if not(r['configuration_id']=='b' and r['case_id']=='test-b')]
        report=summarise(self.dataset,changed)
        group=next(g for g in report['groups'] if g['configuration_id']=='b' and g['split']=='test')
        self.assertEqual((group['unknown'],group['complete_families'],group['passed_families']),(1,0,0))

    def test_duplicate_or_foreign_rows_cannot_cherry_pick_a_success(self):
        changed=deepcopy(self.observations);changed['records'].append(deepcopy(changed['records'][0]))
        with self.assertRaises(ValueError):summarise(self.dataset,changed)
        changed=deepcopy(self.observations);changed['records'][0]['case_id']='foreign'
        with self.assertRaises(ValueError):summarise(self.dataset,changed)

    def test_unknown_is_never_free_success_and_truncated_text_cannot_pass(self):
        changed=deepcopy(self.observations);row=changed['records'][0];row.update(verdict='unknown',contract_status='failed',finish_reason='length')
        report=summarise(self.dataset,changed)
        self.assertEqual(report['groups'][0]['unknown'],1)
        row['verdict']='pass'
        with self.assertRaises(ValueError):summarise(self.dataset,changed)

    def test_profile_and_effort_revisions_are_not_mixed(self):
        for key,value in [('profile_sha256','0'*64),('effort','high'),('target_matches',False)]:
            changed=deepcopy(self.observations);changed['records'][0][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):summarise(self.dataset,changed)

    def test_dataset_binding_and_splits_are_frozen(self):
        changed=deepcopy(self.dataset);changed['cases'][1]['conversation']='changed prompt'
        with self.assertRaises(ValueError):summarise(changed,self.observations)
        changed=deepcopy(self.dataset);changed['cases'][2]['split']='calibration'
        with self.assertRaises(ValueError):summarise(changed,self.observations)


if __name__=='__main__':unittest.main()
