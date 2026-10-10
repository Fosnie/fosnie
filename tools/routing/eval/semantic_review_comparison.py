"""Shared aggregation for machine diagnostics; native binding belongs to the caller."""
from collections import Counter
import copy

from judge_review import digest
from semantic_review import finalise


def compare(public, private, manifest, plan, observations, indexed, grading_source):
    items = {i['blind_id']: i for i in public['items']}
    reviewers = ('openai', 'gemini-lite')
    reviews, reports = {}, {}
    for reviewer in reviewers:
        rows = []
        for item in public['items']:
            ratings = indexed.get((reviewer, item['blind_id']))
            reason = ('No complete attested answer is available.' if not item['available'] else
                      'Outside the frozen diagnostic sample; not assessed.' if item['case_id'] not in plan['case_ids'] else
                      'Reviewer unavailable, incomplete or contract-invalid; not assessed.')
            rows.append({'blind_id': item['blind_id'], 'ratings': ratings if ratings else [
                {'criterion_id': c['id'], 'outcome': 'unknown', 'rationale': reason} for c in item['criteria']]})
        review = {'schema_version': 1, 'bundle_sha256': digest(manifest),
                  'reviewer': {'kind': 'machine', 'id': reviewer + '-blind-diagnostic', 'blinded_to_metadata': True},
                  'complete': True, 'items': rows}
        reviews[reviewer] = review; reports[reviewer] = finalise(public, private, manifest, review)
    consensus = copy.deepcopy(reviews['openai']); consensus['reviewer']['id'] = 'two-reviewer-conservative-consensus'
    right = {r['blind_id']: {v['criterion_id']: v for v in r['ratings']} for r in reviews['gemini-lite']['items']}
    matrix, criterion_rows = Counter(), []
    for item in consensus['items']:
        original = items[item['blind_id']]
        for left in item['ratings']:
            other = right[item['blind_id']][left['criterion_id']]; a, b = left['outcome'], other['outcome']
            if original['available'] and original['case_id'] in plan['case_ids']:
                matrix[(a, b)] += 1
                criterion_rows.append({'blind_id': item['blind_id'], 'criterion_id': left['criterion_id'],
                                       'openai': a, 'gemini-lite': b, 'consensus': a if a == b else 'unknown'})
            left['outcome'] = a if a == b else 'unknown'
            left['rationale'] = ('Agreement between two machine reviewers; ' if a == b else 'Machine reviewers disagree; outcome remains unknown. ') + \
                                'OpenAI: ' + left['rationale'][:800] + ' Gemini: ' + other['rationale'][:800]
    reviews['consensus'] = consensus; reports['consensus'] = finalise(public, private, manifest, consensus)
    lookup = {i['blind_id']: i for i in private['items']}; sample_groups = []
    for name, review in reviews.items():
        counts = {}
        for item in review['items']:
            hidden = lookup[item['blind_id']]
            if hidden['case_id'] not in plan['case_ids']: continue
            required = {c['id'] for c in items[item['blind_id']]['criteria'] if c['required']}
            ratings = [r['outcome'] for r in item['ratings'] if r['criterion_id'] in required]
            outcome = 'fail' if 'fail' in ratings else 'unknown' if 'unknown' in ratings else 'pass'
            counts.setdefault(hidden['strategy'], Counter())[outcome] += 1
        sample_groups.extend({'reviewer': name, 'strategy': strategy, 'cases': sum(count.values()),
                              **{o: count[o] for o in ('pass', 'fail', 'unknown')}} for strategy, count in counts.items())
    return reviews, {'schema_version': 1, 'grading_source': grading_source, 'saved_answer_source': private['report']['source'],
        'plan_sha256': digest(plan), 'observations_sha256': digest(observations), 'sample_case_ids': plan['case_ids'],
        'sample_groups': sample_groups, 'criterion_agreement': [
            {'openai': a, 'gemini-lite': b, 'count': matrix[(a, b)]} for a in ('pass', 'fail', 'unknown') for b in ('pass', 'fail', 'unknown')],
        'criterion_rows': criterion_rows, 'assessed_slots_per_reviewer': {name: sum(k[0] == name for k in indexed) for name in reviewers},
        'full_cohort_reports': reports, 'machine_review_json_complete': True, 'full_cohort_semantically_assessed': False,
        'real_human_review': False, 'model_authorship_blinding_imperfect': True, 'reviewer_self_preference_tested': False,
        'independent_ground_truth': False, 'quality_evidence': False, 'p12_eligible': False,
        'baseline_response_regenerated': False, 'policy_updated': False}
