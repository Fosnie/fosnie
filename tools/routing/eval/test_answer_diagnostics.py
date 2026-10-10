import unittest
from answer_diagnostics import diagnose


class AnswerDiagnosticsTest(unittest.TestCase):
    def test_format_hints_never_rewrite_strict_score(self):
        for answer,key in [('Answer: (A)','known_prefix_reference_match'),('```text\n(A)\n```','fenced_reference_match'),
                           ('"(A)"','json_string_reference_match'),('London','option_text_reference_match')]:
            with self.subTest(answer=answer):
                result=diagnose(answer,'(A)','Select a city.\n(A) London\n(B) Glasgow')
                self.assertTrue(result[key]);self.assertFalse(result['strict_reference_match']);self.assertFalse(result['score_rewritten'])
    def test_wrong_boolean_or_option_is_distinct_from_shape(self):
        result=diagnose('True','False','False and not False and False is')
        self.assertTrue(result['other_enumerated_value_match']);self.assertFalse(result['casefold_match'])
        result=diagnose('Glasgow','(A)','(A) London\n(B) Glasgow')
        self.assertTrue(result['other_enumerated_value_match']);self.assertFalse(result['option_text_reference_match'])
    def test_only_outer_whitespace_is_strictly_accepted(self):
        self.assertTrue(diagnose('  False\n','False','question')['strict_reference_match'])
        result=diagnose('false','False','question');self.assertTrue(result['casefold_match']);self.assertFalse(result['strict_reference_match'])
    def test_unbounded_or_nontext_is_refused(self):
        for content in [None,{},'x'*(1024*1024+1)]:
            with self.assertRaises(ValueError):diagnose(content,'answer','question')


if __name__=='__main__':unittest.main()
