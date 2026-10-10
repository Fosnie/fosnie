"""Fixed native-provider allocation guard; no retries, implicit budget or credential persistence."""
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import threading
import time

import httpx
from judge_review import digest, encoded, validate_dataset

MODELS = {'openai': 'gpt-5.4-mini', 'gemini-lite': 'gemini-3.1-flash-lite'}
URLS = {'openai': 'https://api.openai.com/v1/responses',
        'gemini-lite': 'https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent'}
PATHS = {'openai': '/openai/v1/responses',
         'gemini-lite': '/gemini/v1beta/models/gemini-3.1-flash-lite:generateContent'}


def make_cloud_plan(dataset, bank, manifest):
    validate_dataset(dataset)
    cases = sorted(dataset['cases'], key=lambda c: (c['split'] != 'calibration', c['family_id'], c['id']))
    if len(cases) > 12 or bank.get('dataset_sha256') != digest(dataset):
        raise ValueError('bounded matching audited cases required')
    return {'schema_version': 1, 'allocation_id': 'audited-cloud-v13', 'approval_status': 'pending',
            'dataset_sha256': digest(dataset), 'answer_bank_sha256': digest(bank), 'manifest_sha256': digest(manifest),
            'requests': [{'name': name, 'model': model, 'case_id': case['id'], 'split': case['split'],
                          'prompt_sha256': digest(case['conversation'])} for name, model in MODELS.items() for case in cases],
            'maximum_calls': len(cases) * 2, 'maximum_calls_per_model': len(cases), 'maximum_output_tokens': 512,
            'maximum_reserved_tokens_per_model': 50000, 'maximum_input_bytes': 16000,
            'maximum_core_duration_ms': 30000, 'run_duration_seconds': 900, 'gemini_spacing_seconds': 10,
            'core_temperature': {'openai': 0.0, 'gemini-lite': 1.0},
            'wire_temperature': {'openai': 'provider_default', 'gemini-lite': 1.0}, 'effort': None,
            'system_message': None, 'retry': False, 'resumable': False, 'quality_evidence': False,
            'fresh_holdout': False, 'policy_updated': False, 'network_calls': 0}


REASONING_CASE_IDS = {'bbh/dyck_languages/44', 'bbh/dyck_languages/54',
                      'bbh/multistep_arithmetic_two/14', 'bbh/multistep_arithmetic_two/142'}


def make_reasoning_plan(dataset, bank, manifest):
    base = make_cloud_plan(dataset, bank, manifest)
    cases = sorted((c for c in dataset['cases'] if c['id'] in REASONING_CASE_IDS), key=lambda c: (c['family_id'], c['id']))
    if {c['id'] for c in cases} != REASONING_CASE_IDS or any(c['split'] != 'test' for c in cases):
        raise ValueError('four fixed inherited reasoning cases required')
    return {**base, 'allocation_id': 'audited-reasoning-v14', 'maximum_calls': 16, 'maximum_calls_per_model': 8,
            'maximum_output_tokens': 2048, 'comparison': 'default versus high, same2048 output ceiling',
            'effort': {'default': None, 'high': 'high'},
            'wire_temperature': {'openai': 'provider_default', 'gemini-lite': '1.0 explicit/default'},
            'requests': [{'name': name, 'model': model, 'variant': variant, 'effort': None if variant == 'default' else 'high',
                          'case_id': case['id'], 'split': case['split'], 'prompt_sha256': digest(case['conversation'])}
                         for name, model in MODELS.items() for variant in ('default', 'high') for case in cases]}


def expected_body(name, case, *, output_tokens=512, effort=None):
    if name == 'openai':
        body = {'model': MODELS[name], 'input': [{'role': 'user', 'content': case['conversation']}],
                'max_output_tokens': output_tokens, 'stream': False}
        if effort is not None:body['reasoning'] = {'effort': effort}
        return body
    if name == 'gemini-lite':
        gen = {'maxOutputTokens': output_tokens, 'temperature': 1.0}
        if effort is not None:
            if effort != 'high':raise ValueError('fixed reasoning variant required')
            gen.pop('temperature');gen['thinkingConfig'] = {'thinkingLevel': 'HIGH', 'includeThoughts': True}
        return {'contents': [{'role': 'user', 'parts': [{'text': case['conversation']}]}], 'generationConfig': gen}
    raise ValueError('fixed provider required')


class BoundedCloudProxy:
    def __init__(self, keys, plan, dataset, bank, manifest, ledger, *, execute=False, simulation=False,
                 transport=None, clock=time.monotonic):
        factory = make_reasoning_plan if plan.get('allocation_id') == 'audited-reasoning-v14' else make_cloud_plan
        if plan != factory(dataset, bank, manifest):
            raise ValueError('exact frozen cloud plan required')
        if simulation and type(transport) is not httpx.MockTransport or not simulation and transport is not None:
            raise ValueError('simulation requires an isolated mock transport')
        if execute and simulation:
            raise ValueError('live and synthetic modes cannot mix')
        if execute and (set(keys) != set(MODELS) or any(not isinstance(k, str) or not k or '\n' in k or '\r' in k for k in keys.values())):
            raise ValueError('two in-memory provider credentials required')
        self.keys = dict(keys);self.plan = plan;self.cases = {c['id']: c for c in dataset['cases']}
        self.execute = execute;self.simulation = simulation;self.transport = transport;self.clock = clock
        self.source = 'synthetic' if simulation else 'live' if execute else 'preflight'
        self.started = clock();self.records = [];self.counts = Counter();self.tokens = Counter();self.blocked = set()
        self.active = None;self.lock = threading.Lock();self.file = Path(ledger).open('xb');self.closed = False
        self.last_gemini = None
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):pass
            def do_POST(self):owner.forward(self)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True);self.thread.start()
        self.base = 'http://127.0.0.1:' + str(self.server.server_port)

    def expect(self, name, case_id, variant=None):
        with self.lock:
            if self.closed or name not in MODELS or case_id not in self.cases:
                raise ValueError('known live allocation slot required')
            if not any(r['name'] == name and r['case_id'] == case_id and r.get('variant') == variant for r in self.plan['requests']):
                raise ValueError('slot is not in the approved recipe')
            if any(r['name'] == name and r['case_id'] == case_id and r.get('variant') == variant for r in self.records):
                raise ValueError('consumed slot cannot be assigned again')
            self.active = (name, case_id, variant)

    def clear(self):
        with self.lock:self.active = None

    def event(self, value):
        self.file.write(encoded(value) + b'\n');self.file.flush();os.fsync(self.file.fileno())

    @staticmethod
    def reply(handler, status, value):
        data = encoded(value);handler.send_response(status);handler.send_header('Content-Type', 'application/json')
        handler.send_header('Content-Length', str(len(data)));handler.end_headers();handler.wfile.write(data)

    def forward(self, handler):
        row = None;started = self.clock()
        try:
            length = int(handler.headers.get('Content-Length', 0))
            if not 0 < length <= self.plan['maximum_input_bytes']:
                return self.reply(handler, 403, {'error': 'allocation_input_bound'})
            def pairs(items):
                result = {}
                for key, value in items:
                    if key in result:raise ValueError('duplicate request key')
                    result[key] = value
                return result
            body = json.loads(handler.rfile.read(length), object_pairs_hook=pairs,
                              parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite request')))
            with self.lock:
                if not self.execute and not self.simulation:
                    return self.reply(handler, 403, {'error': 'allocation_execution_disabled'})
                if self.active is None or self.closed:
                    return self.reply(handler, 403, {'error': 'allocation_unassigned'})
                name, case_id, variant = self.active;case = self.cases[case_id]
                effort = 'high' if variant == 'high' else None
                output_cap = self.plan['maximum_output_tokens']
                if handler.path != PATHS[name] or encoded(body) != encoded(expected_body(name, case, output_tokens=output_cap, effort=effort)):
                    return self.reply(handler, 403, {'error': 'allocation_exact_native_contract'})
                reserve = length + output_cap + 1024;elapsed = self.clock() - self.started
                if (name in self.blocked or self.counts[name] >= self.plan['maximum_calls_per_model']
                    or len(self.records) >= self.plan['maximum_calls']
                    or any(r['name'] == name and r['case_id'] == case_id and r.get('variant') == variant for r in self.records)
                    or self.tokens[name] + reserve > self.plan['maximum_reserved_tokens_per_model']
                    or elapsed >= self.plan['run_duration_seconds']):
                    return self.reply(handler, 429, {'error': 'allocation_exhausted'})
                if name == 'gemini-lite' and self.last_gemini is not None and self.clock() - self.last_gemini < self.plan['gemini_spacing_seconds']:
                    return self.reply(handler, 429, {'error': 'allocation_spacing'})
                headers = {'Authorization': 'Bearer ' + self.keys.get(name, 'synthetic-token')} if name == 'openai' else {
                    'x-goog-api-key': self.keys.get(name, 'synthetic-token')}
                row = {'index': len(self.records), 'allocation_id': self.plan['allocation_id'], 'allocation_sha256': digest(self.plan),
                       'source': self.source, 'name': name, 'model': MODELS[name], 'case_id': case_id,
                       'request_sha256': digest(body), 'input_exactly_verified': True, 'input_bytes': length,
                       'output_cap': output_cap, 'reserved_token_upper_bound': reserve, 'status': 'dispatched', 'usage': None}
                if variant is not None:row.update(variant=variant, effort=effort)
                self.event({'event': 'dispatch', **row})
                self.records.append(row);self.counts[name] += 1;self.tokens[name] += reserve
                self.active = None
                if name == 'gemini-lite':self.last_gemini = self.clock()
            started = self.clock()
            with httpx.Client(timeout=25, trust_env=False, follow_redirects=False, transport=self.transport) as client:
                with client.stream('POST', URLS[name], json=body, headers=headers) as response:
                    row['http_status'] = response.status_code
                    if not response.is_success:
                        row['status'] = 'failed_usage_unknown'
                        if response.status_code in (401, 403, 429):
                            with self.lock:self.blocked.add(name)
                        return self.reply(handler, response.status_code, {'error': 'allocation_upstream_refused'})
                    data = bytearray()
                    for chunk in response.iter_bytes():
                        data.extend(chunk)
                        if len(data) > 1024 * 1024:raise ValueError('response bound')
                value = json.loads(data);usage = value.get('usage') or value.get('usageMetadata')
                if isinstance(usage, dict):
                    fields = ('input_tokens', 'output_tokens', 'total_tokens', 'promptTokenCount', 'candidatesTokenCount', 'totalTokenCount', 'thoughtsTokenCount', 'cachedContentTokenCount')
                    row['usage'] = {k: usage[k] for k in fields if type(usage.get(k)) is int and 0 <= usage[k] <= 10**12} or None
                    details = usage.get('output_tokens_details') or {}
                    if row['usage'] and isinstance(details, dict) and type(details.get('reasoning_tokens')) is int and 0 <= details['reasoning_tokens'] <= 10**12:
                        row['usage']['reasoning_tokens'] = details['reasoning_tokens']
                row['status'] = 'completed' if row['usage'] else 'completed_usage_unknown'
                self.reply(handler, response.status_code, value)
        except Exception:
            if row is not None:row['status'] = 'transport_failed_usage_unknown'
            try:self.reply(handler, 502, {'error': 'allocation_transport_failed'})
            except Exception:pass
        finally:
            if row is not None:
                row['elapsed_ms'] = round((self.clock() - started) * 1000)
                with self.lock:self.event({'event': 'finished', **row})

    def close(self):
        with self.lock:self.active = None;self.closed = True
        self.server.shutdown();self.server.server_close();self.thread.join(timeout=3)
        self.file.close();self.keys.clear()
