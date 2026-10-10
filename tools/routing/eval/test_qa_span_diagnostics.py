import unittest
from qa_span_diagnostics import diagnose


class SpanDiagnosticTests(unittest.TestCase):
    def test_longer_grounded_span_stays_hint_not_score(self):
        ref = {'context': 'The report highlights the risk of death.', 'question': 'What risk?', 'answers': [{'text': 'death'}]}
        result = diagnose(ref, 'the risk of death')
        self.assertTrue(result['literal_context_substring']); self.assertTrue(result['contains_gold_token_sequence'])
        self.assertFalse(result['normalised_gold_exact_match']); self.assertFalse(result['score_rewritten'])

    def test_other_published_context_value_does_not_establish_correctness(self):
        ref = {'context': 'Streams can use IP or ISDN.', 'question': 'What is one network?', 'answers': [{'text': 'IP'}]}
        result = diagnose(ref, 'ISDN')
        self.assertTrue(result['indefinite_question_hint']); self.assertTrue(result['literal_context_substring'])
        self.assertFalse(result['contains_gold_token_sequence']); self.assertFalse(result['question_answer_entailment_verified'])

    def test_no_token_substring_or_empty_value_false_positive(self):
        ref = {'context': 'This is a carpet.', 'question': 'Which object?', 'answers': [{'text': 'car'}]}
        self.assertFalse(diagnose(ref, 'car')['normalised_context_token_sequence'])
        self.assertFalse(diagnose(ref, '')['assessable_value'])


if __name__ == '__main__': unittest.main()
