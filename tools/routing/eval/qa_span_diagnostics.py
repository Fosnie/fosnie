"""Lexical context/span hints, never alternative scores or semantic adjudication."""
import re
from qa_span_answers import normalize


def contains_tokens(whole, part):
    return bool(part) and (' ' + part + ' ') in (' ' + whole + ' ')


def diagnose(reference, value):
    if not isinstance(value, str) or not value.strip() or len(value) > 256:
        return {'diagnostic_only': True, 'score_rewritten': False, 'assessable_value': False}
    predicted = normalize(value); context = reference['context']; context_norm = normalize(context)
    gold = [normalize(a['text']) for a in reference['answers']]
    return {'diagnostic_only': True, 'score_rewritten': False, 'assessable_value': True,
        'literal_context_substring': value in context,
        'normalised_context_token_sequence': contains_tokens(context_norm, predicted),
        'normalised_gold_exact_match': predicted in gold,
        'contains_gold_token_sequence': any(contains_tokens(predicted, target) for target in gold),
        'is_shorter_gold_token_sequence': any(contains_tokens(target, predicted) for target in gold),
        'indefinite_question_hint': bool(re.search(r'\b(?:one|an? example|one of|any)\b', reference['question'], re.I)),
        'question_answer_entailment_verified': False}
