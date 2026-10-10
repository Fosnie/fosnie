# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
import os
from uuid import uuid4
import httpx
import pytest
from fastapi import HTTPException
from app import contextual,embeddings,http_client,llm,metadata,rag_ctx
from app.config import settings
from app.inference_target import records
from app.inference_budget import BudgetExhausted
from app.paths import safe_path
from test_inference_target import target,clean_context,anyio_backend


def envelope(calls=2):
    return {'schema_version':1,'decision_id':str(uuid4()),'purpose':'embed',
        'assignments':{'utility_summary':target(name='ingest-helper')},
        'budget':{'id':str(uuid4()),'timeout_ms':1000,'allowances':{
            'utility_summary':{'max_calls':calls,'input_tokens':16384,'output_tokens':512}}}}


@pytest.mark.anyio
async def test_date_and_context_helpers_use_their_pinned_purpose_and_account_each_call(monkeypatch):
    seen=[]
    def respond(request):
        body=json.loads(request.content);seen.append(body)
        return httpx.Response(200,json={'choices':[{'message':{'content':'NONE' if len(seen)==1 else 'Public fixture section'}}],
                                      'usage':{'prompt_tokens':20,'completion_tokens':2}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        rag_ctx.set_overrides({'routing':envelope()})
        assert await metadata.extract_effective_date([(1,'Public fixture')],'text/plain','fixture.txt') is None
        assert await contextual.contextualise('Public fixture','section')=='Public fixture section'
    assert len(seen)==2 and all(row['model']=='ingest-helper' for row in seen)
    assert all(row['purpose']=='utility_summary' and row['status']=='completed' for row in records())
    assert llm._stage_var.get() is None


@pytest.mark.anyio
async def test_routed_metadata_failure_does_not_silently_downgrade_to_legacy_fallback(monkeypatch):
    async def refuse(*args,**kwargs):raise BudgetExhausted('synthetic allowance')
    monkeypatch.setattr(llm,'complete',refuse)
    rag_ctx.set_overrides({'routing':envelope()})
    with pytest.raises(BudgetExhausted):await metadata.extract_effective_date([(1,'Date 2025-01-01')],'text/plain','fixture.txt')
    with pytest.raises(BudgetExhausted):await contextual.contextualise('fixture','section')
    assert llm._stage_var.get() is None
    rag_ctx.set_overrides({})
    assert await metadata.extract_effective_date([(1,'Date 2025-01-01')],'text/plain','fixture.txt')=='2025-01-01'
    assert await contextual.contextualise('fixture','section')==''


@pytest.mark.skipif(os.name!='nt',reason='Windows canonical path spelling')
def test_windows_extended_paths_keep_storage_identity_and_reject_escape(monkeypatch,tmp_path):
    root=tmp_path/'inside';root.mkdir();file=root/'a.txt';file.write_text('fixture');outside=tmp_path/'outside.txt';outside.write_text('fixture')
    monkeypatch.setattr(settings,'storage_root',str(root))
    prefix=chr(92)*2+'?'+chr(92)
    assert safe_path(prefix+str(file))==str(file.resolve())
    assert safe_path(prefix+str(root/'new.txt'))==str((root/'new.txt').resolve())
    with pytest.raises(HTTPException) as error:safe_path(prefix+str(outside))
    assert error.value.status_code==400


@pytest.mark.anyio
async def test_connected_ingest_returns_both_reports_from_the_inference_owning_task(monkeypatch):
    from app.main import app,ingest_mod
    monkeypatch.setattr(settings,'shared_secret','ingestion-test')
    assignment=envelope(calls=1)
    fixed={'schema_version':1,'provider_id':str(uuid4()),'provider_revision':1,'profile_revision':1,'role':'embed',
        'model':'ingest-embed','base_url':'https://embed.test/v1','api_key':'','active_index_revision':1,
        'embedding_dimension':3,'embedding_collection':'fixture'}
    assignment['fixed_assignments']={'embed':fixed}
    assignment['fixed_budget']={'id':str(uuid4()),'timeout_ms':1000,'allowances':{
        'embed':{'max_calls':1,'input_bytes':8192,'output_bytes':8192}}}
    async def work(*args,**kwargs):
        date=await metadata.extract_effective_date([(1,'Public fixture')],'text/plain','fixture.txt')
        assert await embeddings.embed(['Public fixture'])==[[1.0,0.0,0.0]]
        return {'chunks':1,'effective_date':date}
    monkeypatch.setattr(ingest_mod,'ingest_document',work)
    seen=[]
    def respond(request):
        body=json.loads(request.content);seen.append(body['model'])
        if body['model']=='ingest-embed':return httpx.Response(200,json={'data':[{'index':0,'embedding':[1.0,0.0,0.0]}]})
        return httpx.Response(200,json={'choices':[{'message':{'content':'NONE'}}],'usage':{'prompt_tokens':20,'completion_tokens':1}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client,'get_client',lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app),base_url='http://ml.test') as api:
            response=await api.post('/ingest',headers={'x-fosnie-ml-key':'ingestion-test'},json={
                'doc_id':str(uuid4()),'kb_id':str(uuid4()),'path':'fixture.txt','dimension':3,'overrides':{'routing':assignment}})
    assert response.status_code==200
    value=response.json();assert value['chunks']==1
    assert value['routing_report']['complete'] and value['fixed_routing_report']['complete']
    assert value['routing_report']['calls'][0]['purpose']=='utility_summary'
    assert value['fixed_routing_report']['calls'][0]['role']=='embed'
    assert seen==['ingest-helper','ingest-embed']
