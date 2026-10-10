# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4
import httpx
import pytest
from app import chunker,contextual,embeddings,extract,http_client,ingest,metadata,qdrant_store,rag_ctx,sparse
from app.config import settings
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context,anyio_backend,target


def routed(calls=8,dual=True):
    def fixed(role,collection):
        return dict(schema_version=1,provider_id=str(uuid4()),provider_revision=1,profile_revision=1,
            role=role,model=role,base_url='https://'+role+'.test/v1',api_key='',embedding_dimension=3,
            embedding_collection=collection,**({'desired_index_revision':2} if role=='embed_desired' else {'active_index_revision':1}))
    values={'embed':fixed('embed','active')}
    if dual:values['embed_desired']=fixed('embed_desired','desired')
    return dict(schema_version=1,decision_id=str(uuid4()),purpose='embed',assignments={'utility_summary':target(name='helper')},
        budget=dict(id=str(uuid4()),timeout_ms=5000,allowances={'utility_summary':dict(max_calls=calls,input_tokens=16384,output_tokens=512)}),
        fixed_assignments=values,fixed_budget=dict(id=str(uuid4()),timeout_ms=5000,allowances={
            role:dict(max_calls=4,input_bytes=8192,output_bytes=8192) for role in values}))


def setup_pipeline(monkeypatch,calls=8,contextualised=False,children=2):
    values=routed(calls)
    rag_ctx.set_overrides({'routing':values,'parent_child':True})
    monkeypatch.setattr(settings,'contextual_retrieval',contextualised)
    async def pages(*args):return [(1,'A public fixture source')]
    monkeypatch.setattr(extract,'extract_pages_ocr',pages)
    monkeypatch.setattr(chunker,'chunk_hierarchy',lambda *args:([
        {'parent_id':'parent','text':'Original parent'}],[{'text':'Original '+str(i),'page_number':1,'clause_section_ref':None,'parent_id':'parent'} for i in range(children)]))
    monkeypatch.setattr(sparse,'sparse_embed',lambda rows:[{'indices':[1],'values':[1.0]} for _ in rows])
    effects=[]
    async def alias(*args,**kwargs):return 'active'
    monkeypatch.setattr(qdrant_store,'alias_target',alias)
    for name in ('create_kb_collection','delete_doc','ensure_parents','upsert_parents','upsert_named','upsert_toc'):
        def make(effect):
            async def record(*args,**kwargs):effects.append((effect,args,kwargs))
            return record
        monkeypatch.setattr(qdrant_store,name,make(name))
    desired=values['fixed_assignments']['embed_desired']
    return effects,{'dim':3,'model':desired['model'],'base_url':desired['base_url'],'api_key':''}


@pytest.mark.anyio
@pytest.mark.parametrize('failure',['summary','active','desired'])
async def test_inference_failure_leaves_old_indexes_and_parents_untouched(monkeypatch,failure):
    effects,dual=setup_pipeline(monkeypatch,contextualised=True)
    seen=[]
    def reply(request):
        body=json.loads(request.content);model=body['model'];seen.append(model)
        bad=(failure=='summary' and model=='helper' or failure=='active' and model=='embed' or failure=='desired' and model=='embed_desired')
        if bad:return httpx.Response(503,json={'error':'fixture'})
        if model=='helper':return httpx.Response(200,json={'choices':[{'message':{'content':'NONE'}}],'usage':{'prompt_tokens':1,'completion_tokens':1}})
        return httpx.Response(200,json={'data':[{'index':i,'embedding':[1.0,0.0,0.0]} for i in range(len(body['input']))]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        with pytest.raises(httpx.HTTPStatusError):await ingest.ingest_document('doc','kb','a.txt','text/plain',3,dual)
    assert effects==[]
    assert 'embed_desired' not in seen if failure!='desired' else 'embed_desired' in seen


@pytest.mark.anyio
async def test_dual_replacement_deletes_both_old_sets_before_shared_parents_and_upserts(monkeypatch):
    effects,dual=setup_pipeline(monkeypatch,contextualised=True)
    def reply(request):
        body=json.loads(request.content)
        if body['model']=='helper':return httpx.Response(200,json={'choices':[{'message':{'content':'NONE'}}],'usage':{'prompt_tokens':1,'completion_tokens':1}})
        assert not effects, 'all inference precedes corpus replacement'
        return httpx.Response(200,json={'data':[{'index':i,'embedding':[1.0,0.0,0.0]} for i in range(len(body['input']))]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        result=await ingest.ingest_document('doc','kb','a.txt','text/plain',3,dual)
    assert result['chunks']==2
    deletes=[v for v in effects if v[0]=='delete_doc']
    assert [v[2]['collection'] for v in deletes]==['active','desired']
    parent_write=next(i for i,v in enumerate(effects) if v[0]=='upsert_parents')
    assert all(effects.index(v)<parent_write for v in deletes)
    assert [v[1][0] for v in effects if v[0]=='upsert_named']==['active','desired']


@pytest.mark.anyio
async def test_contextual_fanout_is_refused_before_metadata_or_embedding_calls(monkeypatch):
    effects,dual=setup_pipeline(monkeypatch,calls=2,contextualised=True,children=2)
    async def unexpected(*args,**kwargs):raise AssertionError('inference before fan-out preflight')
    monkeypatch.setattr(metadata,'extract_effective_date',unexpected)
    monkeypatch.setattr(embeddings,'embed',unexpected)
    with pytest.raises(BudgetExhausted):await ingest.ingest_document('doc','kb','a.txt',None,3,dual)
    assert effects==[] and not rag_ctx.cfg('_routing_budget',None).counts


@pytest.mark.anyio
async def test_failed_context_sibling_is_cancelled_and_joined_before_ingestion_returns(monkeypatch):
    effects,dual=setup_pipeline(monkeypatch,contextualised=True)
    waiting=asyncio.Event();cancelled=asyncio.Event()
    async def date(*args):return None
    async def context(doc,text):
        if text.endswith('0'):
            await waiting.wait();raise BudgetExhausted('fixture sibling failure')
        waiting.set()
        try:await asyncio.sleep(30)
        finally:cancelled.set()
    monkeypatch.setattr(metadata,'extract_effective_date',date)
    monkeypatch.setattr(contextual,'contextualise',context)
    with pytest.raises(BudgetExhausted):await ingest.ingest_document('doc','kb','a.txt',None,3,dual)
    assert cancelled.is_set() and effects==[]


@pytest.mark.parametrize('data',[
    [{'index':0,'embedding':[1,0,0]},{'index':0,'embedding':[1,0,0]}],
    [{'index':True,'embedding':[1,0,0]},{'index':1,'embedding':[1,0,0]}],
    [{'index':0,'embedding':[float('nan'),0,0]},{'index':1,'embedding':[1,0,0]}],
    [{'index':0,'embedding':[True,0,0]},{'index':1,'embedding':[1,0,0]}],
    [{'index':0,'embedding':[1,0]},{'index':1,'embedding':[1,0,0]}],
])
def test_malformed_embeddings_are_rejected_before_corpus_writes(data):
    with pytest.raises(ValueError):embeddings._validate_vectors(data,2,3)


def test_docx_extension_keeps_native_extraction_when_connector_mime_is_text(monkeypatch,tmp_path):
    import docx
    path=tmp_path/'source.docx';document=docx.Document();document.add_paragraph('Native public fixture paragraph');document.save(path)
    text=extract.extract(str(path),'text/plain')
    assert text.strip()=='Native public fixture paragraph'
