from copy import deepcopy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from reference_audit import Unsupported,_ast_value,audit,check,derive,solve
from judge_review import digest,write_new
from public_benchmark import INSTRUCTION


class ReferenceAuditTest(unittest.TestCase):
    def case(self,identity,family,text,split='calibration'):
        return {'id':identity,'family_id':'bbh/'+family,'split':split,'language':'en','conversation':text+INSTRUCTION}
    def setUp(self):
        self.dataset={'schema_version':1,'dataset_id':'parent-fixture','source':{'kind':'synthetic','reference':'contract-only'},'cases':[
            self.case('boolean','boolean_expressions','False and not False and ( False ) is'),
            self.case('anniversary','date_understanding','Jane and John married on Jan 2, 1958. It is their 5-year anniversary today. What is the date one week from today in MM/DD/YYYY?\nOptions:\n(A) 01/09/1961\n(B) 01/09/1963'),
            self.case('unknown','research','A question without a deterministic oracle.','test')]}
        self.bank={'schema_version':1,'dataset_sha256':digest(self.dataset),'validator':'original-v1','cases':[
            {'case_id':case['id'],'family_id':case['family_id'],'split':case['split'],'target':target,'original_input_sha256':'a'*64,'source_url':'https://public.example/source','source_sha256':'b'*64}
            for case,target in zip(self.dataset['cases'],('False','(A)','unknown'))]}
        self.provenance={'dataset_sha256':digest(self.dataset),'answer_bank_sha256':digest(self.bank)}

    def test_disputed_calendar_label_gets_proof_without_mutating_source(self):
        before=deepcopy((self.dataset,self.bank,self.provenance));report=audit(self.dataset,self.bank,self.provenance)
        rows={r['case_id']:r for r in report['rows']}
        self.assertEqual(rows['boolean']['status'],'verified')
        self.assertEqual((rows['anniversary']['status'],rows['anniversary']['computed_target']),('correction_proposed','(B)'))
        self.assertEqual(rows['unknown']['status'],'unverified')
        self.assertEqual((self.dataset,self.bank,self.provenance),before)

    def test_derived_version_keeps_lineage_exposure_and_separates_format_from_values(self):
        report=audit(self.dataset,self.bank,self.provenance)
        dataset,bank,manifest,lineage=derive(self.dataset,self.bank,self.provenance,report,'derived-v2')
        self.assertEqual(len(dataset['cases']),2)
        self.assertIn('only its label',dataset['cases'][1]['conversation'])
        self.assertEqual(bank['cases'][1]['target'],'(B)')
        self.assertEqual(lineage[1]['source_target'],'(A)')
        self.assertTrue(manifest['exposure_inherited']);self.assertFalse(manifest['fresh_holdout'])
        self.assertFalse(manifest['quality_evidence']);self.assertFalse(manifest['human_reviewed'])
        self.assertNotEqual(digest(dataset),digest(self.dataset))
        self.assertEqual(manifest['excluded_case_ids'],['unknown'])

    def test_modified_foreign_or_stale_audit_is_rejected(self):
        report=audit(self.dataset,self.bank,self.provenance)
        for field,value in [('computed_target','(A)'),('status','verified')]:
            changed=deepcopy(report);changed['rows'][1][field]=value
            with self.subTest(field=field),self.assertRaises(ValueError):derive(self.dataset,self.bank,self.provenance,changed,'derived-v2')
        changed=deepcopy(report);changed['parent_dataset_sha256']='0'*64
        with self.assertRaises(ValueError):derive(self.dataset,self.bank,self.provenance,changed,'derived-v2')
        with self.assertRaises(ValueError):derive(self.dataset,self.bank,self.provenance,report,'parent-fixture')

    def test_asts_are_bounded_and_never_execute_calls_names_or_attributes(self):
        self.assertEqual(_ast_value('((-5 - 4 * -8 + 8) * (4 + 3 - 9 * 7))','integer'),-1960)
        self.assertFalse(_ast_value('False and not False','boolean'))
        for text in ['__import__("os").system("bad")','a.b','True[0]','2 ** 999999','9999999999999999999']:
            with self.subTest(text=text),self.assertRaises(Unsupported):_ast_value(text,'integer')

    def test_stack_and_navigation_do_not_assume_fetched_target_is_correct(self):
        value=solve(self.case('brackets','dyck_languages','Complete the rest of the sequence, making sure that the parentheses are closed properly. Input: < [ [ ( )'))
        self.assertEqual(value[0],'] ] >')
        text='If you follow these instructions, do you return to the starting point? Take 2 steps. Turn around. Take 2 steps.\nOptions:\n- Yes\n- No'
        self.assertEqual(solve(self.case('return','navigate',text))[0],'Yes')
        text='If you follow these instructions, do you return to the starting point? Always face forward. Take 9 steps left. Take 5 steps right.\nOptions:\n- Yes\n- No'
        self.assertEqual(solve(self.case('offset','navigate',text))[0],'No')

    def test_ambiguous_or_unsupported_grammar_stays_unverified(self):
        examples=[self.case('leap','date_understanding','Jane was born on the last day of Feburary in 2000. Today is her 16-year-old birthday. What is the date a month ago in MM/DD/YYYY?\nOptions:\n(A) 01/29/2016'),
                  self.case('bad','dyck_languages','Complete the rest of the sequence, making sure that the parentheses are closed properly. Input: ( ]'),
                  self.case('unknownmove','navigate','If you follow these instructions, do you return to the starting point? Teleport home.\nOptions:\n- Yes\n- No')]
        for case in examples:
            with self.subTest(case=case['id']),self.assertRaises(Unsupported):solve(case)

    def test_format_directives_never_contain_the_case_answer(self):
        case=self.case('words','word_sorting','Sort the following words alphabetically: List: zeta alpha beta')
        self.assertEqual(solve(case)[0],'alpha beta zeta')
        from reference_audit import DIRECTIVES
        self.assertNotIn('alpha beta zeta',DIRECTIVES['words'])

    def test_packaged_lineage_and_prompt_tampering_are_checked(self):
        report=audit(self.dataset,self.bank,self.provenance)
        result=derive(self.dataset,self.bank,self.provenance,report,'derived-v2')
        with tempfile.TemporaryDirectory() as temp:
            parent=Path(temp)/'parent';parent.mkdir();(parent/'LICENSE-BBH.txt').write_text('synthetic licence fixture')
            output=Path(temp)/'derived';output.mkdir()
            for name,value in [('prompts',result[0]),('answer-bank',result[1]),('manifest',result[2]),('source-lineage',result[3]),('reference-audit',report)]:
                write_new(output/(name+'.json'),value)
            (output/'LICENSE-BBH.txt').write_bytes((parent/'LICENSE-BBH.txt').read_bytes())
            with patch('reference_audit.public_cohort',return_value=(self.dataset,self.bank,self.provenance)):
                self.assertEqual(check(output,parent),result)
                modified=deepcopy(result[0]);modified['cases'][0]['conversation']='silently changed input'
                (output/'prompts.json').write_text(__import__('json').dumps(modified))
                with self.assertRaises(ValueError):check(output,parent)


if __name__=='__main__':unittest.main()
