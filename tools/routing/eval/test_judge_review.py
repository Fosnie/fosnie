from copy import deepcopy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from judge_diagnostics import analyse
import judge_review as review
import public_benchmark as public


class ReviewTest(unittest.TestCase):
    def setUp(self):
        self.dataset={'schema_version':1,'dataset_id':'unit-cohort','source':{'kind':'synthetic','reference':'synthetic contract fixture'},
            'cases':[{'id':'one','family_id':'fixture/family-one','split':'test','language':'en','conversation':'Read the supplied unit document.'},
                     {'id':'two','family_id':'fixture/family-one','split':'test','language':'ru','conversation':'Прочитай предоставленный тестовый документ.'}]}
        self.frozen=review.freeze(self.dataset)
        self.completed=deepcopy(self.frozen['review'])
        self.completed.update(reviewer_id='synthetic-test-person',blinded_to_observations=True,complete=True)
        for item in self.completed['case_reviews']:
            item.update(rationale='Supplied information needs no external tool.',label={
                'tasks':['extraction'],'minimum_tier':'economy',**{flag:False for flag in review.FLAGS}})

    def test_variants_do_not_inflate_independent_family_count(self):
        labels=review.finalise(self.dataset,self.frozen['manifest'],self.completed)
        raw={'task':'extraction','minimum_tier':'economy','confidence':1.0,**{flag:False for flag in review.FLAGS}}
        observed={'schema_version':1,'models':['local'],'records':[{'model':'local','case_id':'one','status':'completed','classification':raw}]}
        result=analyse(labels,observed)
        self.assertEqual(result['groups'][0]['total'],2)
        self.assertEqual(result['family_groups'][0]['families'],1)
        self.assertEqual(result['family_groups'][0]['matching_families'],0)
        self.assertEqual(result['model_calls'],0)
        self.assertFalse(result['quality_evidence'])
        self.assertFalse(labels['quality_evidence'])
        self.assertEqual(labels['review']['source_kind'],'synthetic')

    def test_review_requires_binding_human_attestation_every_case_and_rationale(self):
        mutations=[('dataset_sha256','0'*64),('manifest_sha256','0'*64),('review_kind','ai'),('blinded_to_observations',False),('complete',False),('reviewer_id','')]
        for key,value in mutations:
            changed=deepcopy(self.completed);changed[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):review.finalise(self.dataset,self.frozen['manifest'],changed)
        for change in ('missing','duplicate','foreign','rationale','boolean','unknown'):
            changed=deepcopy(self.completed)
            if change=='missing':changed['case_reviews'].pop()
            if change=='duplicate':changed['case_reviews'][1]=deepcopy(changed['case_reviews'][0])
            if change=='foreign':changed['case_reviews'][1]['case_id']='foreign'
            if change=='rationale':changed['case_reviews'][0]['rationale']=''
            if change=='boolean':changed['case_reviews'][0]['label']['tools']=0
            if change=='unknown':changed['case_reviews'][0]['label']['tasks']=['unknown']
            with self.subTest(change=change),self.assertRaises(ValueError):review.finalise(self.dataset,self.frozen['manifest'],changed)

    def test_family_leakage_and_normalised_prompt_copy_are_rejected(self):
        changed=deepcopy(self.dataset);changed['cases'][1]['split']='calibration'
        with self.assertRaises(ValueError):review.freeze(changed)
        changed=deepcopy(self.dataset);changed['cases'][1]['conversation']='  READ  THE SUPPLIED UNIT DOCUMENT.\n'
        with self.assertRaises(ValueError):review.freeze(changed)
        exposed={'schema_version':1,'records':[{'family_id':'renamed','prompt_sha256':review.prompt_digest(self.dataset['cases'][0]['conversation'])}]}
        with self.assertRaises(ValueError):review.freeze(self.dataset,[exposed])
        exposed['records'][0].update(family_id=self.dataset['cases'][0]['family_id'],prompt_sha256='0'*64)
        with self.assertRaises(ValueError):review.freeze(self.dataset,[exposed])

    def test_known_ai_calibration_family_cannot_be_renamed_as_heldout(self):
        seed=review.load(review.ROOT/'fixtures/judge-calibration.v1.json')
        changed=deepcopy(self.dataset)
        changed['cases'][0]['family_id']=seed['cases'][0]['label']['family_id']
        with self.assertRaises(ValueError):review.freeze(changed)
        changed['cases'][0]['family_id']='renamed-family'
        changed['cases'][0]['conversation']=seed['cases'][0]['conversation']
        with self.assertRaises(ValueError):review.freeze(changed)

    def test_export_is_blind_inert_and_refuses_overwrite_or_tampering(self):
        changed=deepcopy(self.dataset)
        changed['cases'][0]['conversation']='</script><script>window.__sourceExecuted=true</script>'
        with tempfile.TemporaryDirectory() as temp:
            directory=Path(temp);source=directory/'input.json';review.write_new(source,changed)
            with patch('urllib.request.urlopen',side_effect=AssertionError('unexpected network')):
                review.prepare_files(source,directory/'bundle')
            page=(directory/'bundle/review.html').read_text(encoding='utf-8')
            self.assertNotIn(changed['cases'][0]['conversation'],page)
            self.assertIn('\\u003c/script\\u003e',page)
            self.assertNotIn('proposed_label',page)
            with self.assertRaises(FileExistsError):review.prepare_files(source,directory/'bundle')
            dataset,manifest=review.bundle(directory/'bundle')
            self.assertEqual(dataset,changed)
            tampered=deepcopy(dataset);tampered['cases'][0]['conversation']='silently changed'
            (directory/'bundle/dataset.json').write_text(json.dumps(tampered),encoding='utf-8')
            with self.assertRaises(ValueError):review.bundle(directory/'bundle')

    def test_bounded_loader_rejects_duplicate_keys_and_nonfinite_json(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'input.json'
            for payload in ('{"a":1,"a":2}','{"a":NaN}'):
                path.write_text(payload,encoding='utf-8')
                with self.assertRaises(ValueError):review.load(path)
            path.write_bytes(b' '* (review.LIMIT+1))
            with self.assertRaises(ValueError):review.load(path)

    def test_public_cohort_preserves_targets_canary_revision_and_disjoint_tasks(self):
        documents={task:json.dumps({'canary':'fixture-canary','examples':[{'input':task+' question '+str(i),'target':'answer '+str(i)} for i in range(3)]}).encode()
                   for task in public.TASKS}
        dataset,answers,provenance=public.convert('a'*40,documents)
        self.assertEqual(len(dataset['cases']),46)
        self.assertEqual(len({c['family_id'] for c in dataset['cases'] if c['split']=='test'}),20)
        self.assertEqual(len({c['family_id'] for c in dataset['cases'] if c['split']=='calibration'}),3)
        self.assertEqual(len(answers['cases']),46)
        self.assertEqual(answers['dataset_sha256'],review.digest(dataset))
        self.assertTrue(all(s['canary']=='fixture-canary' for s in provenance['sources']))
        self.assertTrue(all('/'+'a'*40+'/' in c['source_url'] for c in answers['cases']))
        self.assertEqual(public.convert('a'*40,documents),(dataset,answers,provenance))
        with self.assertRaises(ValueError):public.convert('main',documents)
        documents.pop(next(iter(documents)))
        with self.assertRaises(ValueError):public.convert('a'*40,documents)


if __name__=='__main__':unittest.main()
