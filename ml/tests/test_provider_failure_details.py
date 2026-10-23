# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
import httpx
import pytest
from app.llm import LlmError
from app.inference_failure import failure_detail,QUOTA_CODES
from test_inference_target import anyio_backend,clean_context


@pytest.mark.anyio
@pytest.mark.parametrize('code',sorted(QUOTA_CODES))
async def test_billing_codes_distinct_from_plain_429_and_no_raw_body(code):
    response=httpx.Response(429,json={'error':{'code':code,'type':'insufficient_quota','message':'private-key-should-never-escape'}},
        headers={'retry-after':'15'},request=httpx.Request('POST','https://vendor.test'))
    error=await LlmError.from_response(response)
    value=failure_detail(error,emitted=False)
    assert value=={'schema_version':1,'kind':'quota_exhausted','before_output':True,'retry_after_seconds':15}
    assert 'private-key' not in str(error) and 'private-key' not in json.dumps(value)


@pytest.mark.anyio
@pytest.mark.parametrize('status,code,kind',[(429,'slow_down','rate_limited'),(429,None,'rate_limited'),
    (402,None,'payment_required'),(401,None,'unauthorized'),(403,None,'access_denied'),(404,'model_not_found','model_unavailable'),(404,None,'invalid_request'),
    (503,'server_is_overloaded','unavailable'),(529,None,'unavailable'),(400,'context_length_exceeded','invalid_request')])
async def test_native_status_and_allowlisted_code_propagate_with_phase(status,code,kind):
    response=httpx.Response(status,json={'error':{'code':code,'message':'credits gone maybe'}},request=httpx.Request('POST','https://vendor.test'))
    error=await LlmError.from_response(response)
    assert failure_detail(error,emitted=False)['kind']==kind
    assert failure_detail(error,emitted=True)['before_output'] is False


@pytest.mark.anyio
@pytest.mark.parametrize('header',['-1','0','Infinity','NaN','999999','0.5','Wed, 1 Jan 2020','15'])
async def test_retry_after_is_bounded_integer_not_exception_guess(header):
    response=httpx.Response(429,text='secret quota_exhausted 402',headers={'retry-after':header},request=httpx.Request('POST','https://vendor.test'))
    value=failure_detail(await LlmError.from_response(response),emitted=False)
    assert value['kind']=='rate_limited'
    assert value.get('retry_after_seconds')==(15 if header=='15' else None)


@pytest.mark.anyio
async def test_oversized_vendor_error_body_is_not_kept_or_interpreted():
    response=httpx.Response(429,json={'error':{'code':'insufficient_quota','message':'x'*20000}},request=httpx.Request('POST','https://vendor.test'))
    error=await LlmError.from_response(response)
    assert error.upstream_code is None
    assert failure_detail(error,emitted=False)['kind']=='rate_limited'
    assert len(str(error))<100


def test_plain_deadline_and_arbitrary_text_never_authorize_provider_retry():
    for error in (TimeoutError('native maybe?'),ValueError('429 insufficient_quota'),LlmError('402'),RuntimeError('connection failed')):
        assert failure_detail(error,emitted=False) is None
