# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Safe provider failure metadata. No exception text or credentials on the wire."""
import httpx

QUOTA_CODES = frozenset(('insufficient_quota', 'credit_balance_exhausted', 'organization_spend_limit_exceeded',
                         'project_spend_limit_exceeded', 'organization_usage_limit_exceeded'))
PAYMENT_CODES = frozenset(('payment_required',))
ALLOWED_CODES = QUOTA_CODES | PAYMENT_CODES | frozenset(('invalid_api_key', 'authentication_error', 'permission_error',
    'rate_limit_error', 'rate_limit_exceeded', 'slow_down', 'overloaded_error', 'context_length_exceeded', 'model_not_found'))


def http_metadata(response, *, body=None):
    code = None
    try:
        content = response.content if body is None else body
        if len(content) <= 16384:
            value = __import__('json').loads(content)
            error = value.get('error') if isinstance(value, dict) else None
            if isinstance(error, dict):
                raw_code = error.get('code')
                raw_type = error.get('type')
                if isinstance(raw_code, str) and raw_code in ALLOWED_CODES: code = raw_code
                elif isinstance(raw_type, str) and raw_type in ALLOWED_CODES: code = raw_type
    except (ValueError, TypeError, httpx.ResponseNotRead):
        pass
    after = None
    raw = response.headers.get('retry-after', '')
    if raw.isascii() and raw.isdecimal() and len(raw) <= 5:
        seconds = int(raw)
        if 1 <= seconds <= 86400: after = seconds
    return code, after


def failure_detail(error, *, emitted):
    from .llm import LlmError
    status = (error.upstream_status if isinstance(error, LlmError)
              else error.response.status_code if isinstance(error, httpx.HTTPStatusError) else None)
    code = getattr(error, 'upstream_code', None)
    after = getattr(error, 'retry_after_seconds', None)
    if isinstance(error, httpx.HTTPStatusError): code, after = http_metadata(error.response)
    kind = None
    if status == 429 and code in QUOTA_CODES: kind = 'quota_exhausted'
    elif status == 402 or status in (400, 403, 429) and code in PAYMENT_CODES: kind = 'payment_required'
    elif status == 401: kind = 'unauthorized'
    elif status == 403: kind = 'access_denied'
    elif status == 404 and code == 'model_not_found': kind = 'model_unavailable'
    elif status == 429: kind = 'rate_limited'
    elif status in (500, 502, 503, 504, 529): kind = 'unavailable'
    elif status in (400, 404, 422): kind = 'invalid_request'
    elif isinstance(error, (httpx.ConnectError, httpx.ConnectTimeout)): kind = 'connection'
    elif isinstance(error, httpx.TimeoutException): kind = 'timeout'
    if kind is None: return None
    result = {'schema_version': 1, 'kind': kind, 'before_output': not emitted}
    if type(after) is int and 1 <= after <= 86400: result['retry_after_seconds'] = after
    return result


def failure_code(error, *, emitted):
    # Preserve the old helper's terminal-only contract for existing callers/tests.
    if emitted: return None
    value = failure_detail(error, emitted=False)
    if not value: return None
    return {'connection':'connection','timeout':'unavailable','rate_limited':'rate_limited',
            'quota_exhausted':'rate_limited','unavailable':'unavailable'}.get(value['kind'])
