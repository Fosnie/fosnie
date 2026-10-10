"""Versioned wire cardinality fix; V38 outcomes and local quality rules stay frozen."""
import hashlib
from pathlib import Path

from judge_review import digest, encoded
import semantic_catalog_judge as v2
import semantic_judge as v1
import semantic_review as review
import semantic_review_comparison as comparison

MODELS, URLS = v2.MODELS, v2.URLS
catalogue, extract, prepare_assessment = v2.catalogue, v2.extract, v2.prepare_assessment
SYSTEM = v2.SYSTEM + """Return each supplied criterion exactly once, with no extra criterion ratings.
For EACH rating select AT MOST FOUR distinct evidence IDs. Select the most relevant
fragments; do not list every catalogue entry. For pass/fail select at least one ID.
For unknown an empty evidence list is allowed. Never invent an ID or change text.
"""


def fingerprint():
    return digest({'version': 'fosnie-semantic-catalogue-v3', 'v2_protocol_sha256': v2.fingerprint(),
                   'source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})


def schema(item):
    result = v2.schema(item)
    ratings = result['properties']['ratings']
    ratings.update(minItems=len(item['criteria']), maxItems=len(item['criteria']))
    ratings['items']['properties']['evidence_ids']['maxItems'] = 4
    return result


def body(reviewer, item):
    value = v2.body(reviewer, item)
    if reviewer == 'openai':
        value['input'][0]['content'] = SYSTEM; value['text']['format']['schema'] = schema(item)
    else:
        value['systemInstruction']['parts'][0]['text'] = SYSTEM; value['generationConfig']['responseJsonSchema'] = schema(item)
    if len(encoded(value)) > 32768: raise ValueError('catalogue native input bound')
    return value


def make_plan(public, manifest, case_ids):
    plan = v2.make_plan(public, manifest, case_ids)
    items = {i['blind_id']: i for i in public['items']}
    for slot in plan['requests']:
        value = body(slot['reviewer'], items[slot['blind_id']]); size = len(encoded(value))
        slot.update(body_sha256=digest(value), input_bytes=size, token_upper=size + 2048 + 512)
    plan.update(protocol='catalogue-v3', protocol_sha256=fingerprint(),
                maximum_reserved_tokens=sum(s['token_upper'] for s in plan['requests']),
                selection_basis='new public writing case to verify wire cardinality; no V37/V38 reviewer case repeated; exposed diagnostic')
    return plan


def validate_plan(public, manifest, plan):
    if plan != make_plan(public, manifest, plan.get('case_ids')):
        raise ValueError('exact frozen cardinality proposal required')


def diagnostics(public, private, manifest, plan, observations, *, grading_source='synthetic'):
    review.validate_bundle(public, private, manifest); validate_plan(public, manifest, plan)
    if grading_source not in ('live', 'synthetic') or not isinstance(observations, list) or len(observations) > plan['maximum_calls']:
        raise ValueError('invalid cardinality observation source/bound')
    slots = {(s['reviewer'], s['blind_id']): s for s in plan['requests']}
    items = {i['blind_id']: i for i in public['items']}; indexed, seen = {}, set()
    for row in observations:
        review.exact(row, ('reviewer', 'blind_id', 'body_sha256', 'status', 'assessment'), 'invalid cardinality observation')
        key = (row['reviewer'], row['blind_id'])
        if key not in slots or key in seen or row['body_sha256'] != slots[key]['body_sha256'] or row['status'] not in ('completed', 'unknown'):
            raise ValueError('duplicate/foreign cardinality observation or body mismatch')
        seen.add(key)
        if row['status'] == 'completed': indexed[key] = v1.validate_assessment(items[key[1]], row['assessment'])
        elif row['assessment'] is not None: raise ValueError('unknown cardinality observation cannot score')
    reviews, report = comparison.compare(public, private, manifest, plan, observations, indexed, grading_source)
    report['protocol'] = 'catalogue-v3'
    return reviews, report
