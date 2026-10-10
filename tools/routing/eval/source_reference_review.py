"""Explicit operator adjudication of source-only audit; never invent human review."""
import copy
import html
import json

from judge_review import digest, load, validate_dataset, write_new
from qa_span_answers import fingerprint, normalize, validate
import source_reference_audit as audit


def candidates(item, row):
    result = [{'id': 'published/' + str(i), 'text': r['text'], 'answer_start': r['answer_start'],
               'origin': 'upstream_published'} for i, r in enumerate(item['published_answers'])]
    seen = {r['text'] for r in result}
    for span in row['proposed_spans']:
        if span['text'] in seen: continue
        seen.add(span['text'])
        result.append({'id': 'proposal/' + str(len(result)), 'text': span['text'],
            'answer_start': item['context'].find(span['text']), 'origin': 'machine_proposed'})
    return result


def draft(public, report):
    return {'schema_version': 1, 'public_sha256': digest(public), 'audit_report_sha256': digest(report),
        'reviewer': {'kind': 'human', 'id': '', 'source_only_review_attested': False, 'executor_answers_seen': True},
        'cases': [{'case_id': i['case_id'], 'decision': 'unset', 'accepted_reference_ids': [], 'rationale': ''} for i in public['items']]}


def finalise(dataset, bank, public, report, review):
    validate(dataset, bank); audit.validate_public(public)
    if (report.get('source') != 'live' or report.get('published_annotations_changed') is not False
            or report.get('executor_answers_seen') is not False or report.get('p12_eligible') is not False):
        raise ValueError('live source-only audit required')
    if (not isinstance(review, dict) or set(review) != {'schema_version', 'public_sha256', 'audit_report_sha256', 'reviewer', 'cases'}
            or type(review['schema_version']) is not int or review['schema_version'] != 1
            or review['public_sha256'] != digest(public) or review['audit_report_sha256'] != digest(report)):
        raise ValueError('source review binding')
    reviewer = review['reviewer']
    if (not isinstance(reviewer, dict) or set(reviewer) != {'kind', 'id', 'source_only_review_attested', 'executor_answers_seen'}
            or reviewer['kind'] != 'human' or not isinstance(reviewer['id'], str) or not 1 <= len(reviewer['id'].strip()) <= 128
            or reviewer['source_only_review_attested'] is not True or reviewer['executor_answers_seen'] is not False):
        raise ValueError('actual human source-only attestation required')
    items = {i['case_id']: i for i in public['items']}; reports = {r['case_id']: r for r in report['cases']}
    original = {r['case_id']: r for r in bank['cases']}
    if set(items) != set(original) or set(items) != set(reports): raise ValueError('source review cohort mismatch')
    for item in items.values():
        row = original[item['case_id']]
        if any(item[k] != row[k] for k in ('context', 'question')) or item['published_answers'] != row['answers']:
            raise ValueError('source review corpus mismatch')
    if not isinstance(review['cases'], list) or len(review['cases']) != len(items): raise ValueError('complete source review required')
    seen, accepted, provenance = set(), {}, []
    for row in review['cases']:
        if not isinstance(row, dict) or set(row) != {'case_id', 'decision', 'accepted_reference_ids', 'rationale'}:
            raise ValueError('source review case fields')
        identity = row['case_id']; ids = row['accepted_reference_ids']; choice = row['decision']
        if (identity not in items or identity in seen or choice not in ('accept', 'exclude')
                or not isinstance(ids, list) or any(not isinstance(i, str) for i in ids) or len(set(ids)) != len(ids)
                or not isinstance(row['rationale'], str) or not 10 <= len(row['rationale'].strip()) <= 2000):
            raise ValueError('incomplete or duplicate source review case')
        seen.add(identity); options = {r['id']: r for r in candidates(items[identity], reports[identity])}
        if not set(ids) <= set(options) or choice == 'accept' and not 1 <= len(ids) <= 16 or choice == 'exclude' and ids:
            raise ValueError('explicit bounded reference selection required')
        if choice == 'accept':
            refs = [options[i] for i in ids]
            if len({normalize(r['text']) for r in refs}) != len(refs): raise ValueError('duplicate normalised reference')
            for ref in refs:
                start, text = ref['answer_start'], ref['text']
                if start < 0 or items[identity]['context'][start:start + len(text)] != text: raise ValueError('unanchored reviewed reference')
            accepted[identity] = [{'text': r['text'], 'answer_start': r['answer_start']} for r in refs]
            provenance.append({'case_id': identity, 'selected_references': refs, 'rationale': row['rationale']})
    if not accepted: raise ValueError('no accepted source cases')
    output_dataset = copy.deepcopy(dataset)
    output_dataset['dataset_id'] = 'fosnie-source-reviewed-span-v1/' + digest(review)[:16]
    output_dataset['cases'] = [c for c in output_dataset['cases'] if c['id'] in accepted]
    output_dataset['source']['reference'] = 'Public pinned SQuAD1.1 TRAIN; source-only human reference review. Public model-training exposure and independent family/tier review remain unverified.'
    validate_dataset(output_dataset)
    output_bank = copy.deepcopy(bank); output_bank['dataset_sha256'] = digest(output_dataset); output_bank['evaluator_sha256'] = fingerprint()
    output_bank['cases'] = [{**r, 'answers': accepted[r['case_id']]} for r in bank['cases'] if r['case_id'] in accepted]
    validate(output_dataset, output_bank)
    manifest = {'schema_version': 1, 'source_public_sha256': digest(public), 'audit_report_sha256': digest(report),
        'operator_review_sha256': digest(review), 'dataset_sha256': digest(output_dataset), 'answer_bank_sha256': digest(output_bank),
        'reference_authorship': 'upstream plus explicitly selected source-only human adjudication', 'human_attestation_recorded': True,
        'human_attestation_independently_verified': False, 'executor_answers_seen': False, 'reference_provenance': provenance,
        'accepted_cases': len(accepted), 'excluded_cases': len(items) - len(accepted), 'published_originals_retained': True,
        'family_independence_reviewed': False, 'tier_labels_reviewed': False, 'quality_evidence': False, 'p12_eligible': False}
    return output_dataset, output_bank, manifest


def page(public, report):
    template = draft(public, report); rows = {r['case_id']: r for r in report['cases']}
    sections = []
    for item in public['items']:
        row = rows[item['case_id']]
        fields = ''.join('<label><input type="checkbox" value="' + html.escape(c['id'], quote=True) + '"> ' +
            html.escape(c['text']) + ' (' + c['origin'] + ')</label>' for c in candidates(item, row))
        sections.append('<section data-case="' + html.escape(item['case_id'], quote=True) + '"><h2>' + html.escape(item['question']) +
            '</h2><pre>' + html.escape(item['context']) + '</pre><details><summary>Machine suggestions: ' + html.escape(row['decision']) +
            '</summary><pre>' + html.escape(json.dumps(row, ensure_ascii=False, indent=2)) + '</pre></details>' +
            '<select><option value="unset">Choose after reviewing the source</option><option value="accept">Accept selected references</option><option value="exclude">Exclude question</option></select>' +
            fields + '<textarea placeholder="Source-based rationale (at least 10 characters)"></textarea></section>')
    payload = json.dumps(template, ensure_ascii=False).replace('<', '\\u003c')
    return '''<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'"><title>Source-only reference review</title>
<style>body{font:16px system-ui;max-width:1000px;margin:30px auto;padding:20px}section{border-top:1px solid #888;margin:30px 0;padding:20px 0}pre{white-space:pre-wrap}label{display:block;margin:10px 0}textarea{width:95%;height:70px}select,button,input{padding:6px}</style>
<h1>Source-only reference review</h1><p>Review the question and passage. Machine suggestions are unverified. Exclude unresolved ambiguity. Selected references are frozen before executor answers; this does not certify tiers or P12.</p>
<label>Reviewer identity <input id="reviewer"></label><label><input type="checkbox" id="attest"> I personally reviewed these sources and have not seen executor answers to these cases.</label>''' + ''.join(sections) + '''<button id="save">Download review JSON</button><p id="error"></p><script>
const draft = ''' + payload + ''';
document.getElementById('save').onclick=()=>{const id=document.getElementById('reviewer').value.trim();
const rows=[...document.querySelectorAll('section')].map(s=>({case_id:s.dataset.case,decision:s.querySelector('select').value,accepted_reference_ids:[...s.querySelectorAll('input:checked')].map(x=>x.value),rationale:s.querySelector('textarea').value.trim()}));
if(!id || !document.getElementById('attest').checked || rows.some(r=>r.decision==='unset'||r.rationale.length<10||r.decision==='accept'&&!r.accepted_reference_ids.length||r.decision==='exclude'&&r.accepted_reference_ids.length)){document.getElementById('error').textContent='Complete every decision, rationale and personal attestation.';return;}
draft.reviewer={kind:'human',id,source_only_review_attested:true,executor_answers_seen:false};draft.cases=rows;
const u=URL.createObjectURL(new Blob([JSON.stringify(draft,null,2)],{type:'application/json'}));const a=document.createElement('a');a.href=u;a.download='source-review.json';a.click();URL.revokeObjectURL(u);};</script>'''


if __name__ == '__main__':
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', required=True); parser.add_argument('--report', required=True)
    parser.add_argument('--review', required=True); parser.add_argument('--output', required=True)
    args = parser.parse_args(); directory = Path(args.bundle); output = Path(args.output)
    if output.exists(): parser.error('fresh exclusive output required')
    dataset, bank, public = [load(directory / (name + '.json')) for name in ('dataset', 'answer-bank', 'public')]
    review = load(args.review); result = finalise(dataset, bank, public, load(args.report), review)
    output.mkdir()
    for name, value in zip(('dataset', 'answer-bank', 'manifest'), result): write_new(output / (name + '.json'), value)
    write_new(output / 'operator-review.json', review)
