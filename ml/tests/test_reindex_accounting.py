# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
from uuid import uuid4
from types import SimpleNamespace
import httpx,pytest
from app import embeddings,reindex,qdrant_store,rag_ctx,http_client,sparse
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context,anyio_backend


@pytest.fixture(autouse=True)
def shared_root(monkeypatch, tmp_path):
    from app.config import settings
    monkeypatch.setattr(settings, 'storage_root', str(tmp_path))


def envelope(calls=3):
    def target(role,name):return dict(schema_version=1,provider_id=str(uuid4()),provider_revision=1,profile_revision=1,role=role,
        model=name,base_url='https://embed.test/v1',api_key='',embedding_dimension=3,embedding_collection='old' if role=='embed' else 'new',
        **({'active_index_revision':1} if role=='embed' else {'desired_index_revision':2}))
    return dict(schema_version=1,decision_id=str(uuid4()),purpose='embed',assignments={},fixed_assignments={'embed':target('embed','active'),'embed_desired':target('embed_desired','desired')},
        fixed_budget=dict(id=str(uuid4()),timeout_ms=5000,allowances={'embed_desired':dict(max_calls=calls,input_bytes=2*1024*1024,output_bytes=8*1024*1024)}))


def setup(monkeypatch,count=130,calls=3):
    kb,doc=str(uuid4()),str(uuid4());points=[SimpleNamespace(id=str(uuid4()),payload={'knowledge_base_id':kb,'doc_id':doc,'chunk_text':'Public source '+str(i)}) for i in range(count)]
    rag_ctx.set_overrides({'routing':envelope(calls)})
    monkeypatch.setattr(qdrant_store,'alias_target',lambda *a,**k:answer('old'))
    async def scan(*args):yield list(points)
    monkeypatch.setattr(qdrant_store,'scroll_all',scan)
    written=[]
    async def upsert(name,rows,dense,sp):written.extend(rows)
    monkeypatch.setattr(qdrant_store,'upsert_named',upsert)
    monkeypatch.setattr(qdrant_store,'create_kb_collection',lambda *a:answer(None))
    monkeypatch.setattr(qdrant_store,'count',lambda name:answer(len(written)))
    monkeypatch.setattr(sparse,'sparse_embed',lambda texts:[{'indices':[1],'values':[1.0]} for _ in texts])
    class Client:
        async def get_aliases(self):return SimpleNamespace(aliases=[])
        async def collection_exists(self,name):return False
    monkeypatch.setattr(qdrant_store,'client',lambda:Client())
    return kb,doc,points,written
async def answer(value):return value


@pytest.mark.anyio
async def test_whole_rebuild_counts_each_embedding_batch_against_one_fixed_grant(monkeypatch):
    kb,doc,points,written=setup(monkeypatch);seen=[]
    def reply(request):
        body=json.loads(request.content);seen.append(body)
        assert body['model']=='desired'
        return httpx.Response(200,json={'data':[{'index':i,'embedding':[1.0,0.0,0.0]} for i in range(len(body['input']))]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,'get_client',lambda:client)
        events=[e async for e in reindex.reindex_stream(3,'desired',[kb],[doc])]
    assert [len(b['input']) for b in seen]==[64,64,2] and len(written)==130
    assert rag_ctx.cfg('_routing_fixed_budget',None).counts=={'embed_desired':3}
    assert events[-1]['type']=='built' and events[-1]['count']==130


@pytest.mark.anyio
@pytest.mark.parametrize('scope,calls',[('denied',3),('allowed',2)])
async def test_scope_or_grant_overflow_refuses_before_any_embedding(monkeypatch,scope,calls):
    kb,doc,points,written=setup(monkeypatch,calls=calls)
    async def unexpected(*a,**k):raise AssertionError('unplanned inference')
    monkeypatch.setattr(embeddings,'embed_desired',unexpected)
    with pytest.raises((ValueError,BudgetExhausted)):
        _=[e async for e in reindex.reindex_stream(3,'desired',[kb],[doc] if scope=='allowed' else [])]
    assert not written and not rag_ctx.cfg('_routing_fixed_budget',None).counts


@pytest.mark.anyio
async def test_changed_source_stops_publication_without_rebuilding(monkeypatch):
    kb,doc,points,written=setup(monkeypatch,count=1)
    async def embed(texts):points[0].payload['chunk_text']='Changed source';return [[1.0,0.0,0.0]]
    monkeypatch.setattr(embeddings,'embed_desired',embed)
    with pytest.raises(ValueError,match='source changed'):
        _=[e async for e in reindex.reindex_stream(3,'desired',[kb],[doc])]
    assert len(written)==1


@pytest.mark.anyio
async def test_cleanup_refuses_active_and_aliased_collections(monkeypatch):
    monkeypatch.setattr(qdrant_store,'alias_target',lambda *a,**k:answer('active'))
    with pytest.raises(ValueError):await reindex.cleanup_retired('active','active')
    class Client:
        async def get_aliases(self):return SimpleNamespace(aliases=[SimpleNamespace(collection_name='retired')])
    monkeypatch.setattr(qdrant_store,'client',lambda:Client())
    with pytest.raises(ValueError):await reindex.cleanup_retired('retired','active')


@pytest.mark.anyio
async def test_second_live_builder_refuses_before_reset_or_inference(monkeypatch):
    kb, doc, points, written = setup(monkeypatch, count=1)
    seen = []
    async def embed(texts):
        seen.append(texts)
        return [[1.0, 0.0, 0.0]]
    monkeypatch.setattr(embeddings, 'embed_desired', embed)
    first = reindex.reindex_stream(3, 'desired', [kb], [doc])
    assert (await anext(first))['type'] == 'start'
    from app.storage_writes import StorageRecoveryRequired
    with pytest.raises(StorageRecoveryRequired, match='busy'):
        _ = [e async for e in reindex.reindex_stream(3, 'desired', [kb], [doc])]
    assert not seen and not written
    assert [e async for e in first][-1]['type'] == 'built'
    assert len(seen) == 1 and len(written) == 1


@pytest.mark.anyio
async def test_cancelled_generator_releases_builder_lock_for_explicit_attempt(monkeypatch):
    kb, doc, points, written = setup(monkeypatch, count=1)
    first = reindex.reindex_stream(3, 'desired', [kb], [doc])
    await anext(first)
    await first.aclose()
    second = reindex.reindex_stream(3, 'desired', [kb], [doc])
    assert (await anext(second))['type'] == 'start'
    await second.aclose()
    assert not written


@pytest.mark.anyio
@pytest.mark.parametrize('change', ['source', 'target', 'authority'])
async def test_publication_rechecks_receipt_and_authority_inside_storage_fence(monkeypatch, change):
    kb, doc, points, written = setup(monkeypatch, count=1)
    rows, digest = await reindex.corpus_snapshot('old')
    target = [SimpleNamespace(id=points[0].id, payload=dict(points[0].payload))]
    if change == 'source': points[0].payload['chunk_text'] = 'Late edit'
    if change == 'target': target[0].payload['chunk_text'] = 'Contaminated desired corpus'
    async def scan(name): yield points if name == 'old' else target
    monkeypatch.setattr(qdrant_store, 'scroll_all', scan)
    # The target exists: these assertions must reach digest/authority validation,
    # rather than passing merely because the rebuilt collection is absent.
    monkeypatch.setattr(qdrant_store, 'client', lambda: SimpleNamespace(collection_exists=lambda name: answer(True)))
    async def forbidden(*args): raise AssertionError('refused publication changed alias')
    monkeypatch.setattr(qdrant_store, 'swap_alias', forbidden)
    with pytest.raises(reindex.PublicationRefused):
        await reindex.swap('new', 'old', 'old', digest, change != 'authority')


@pytest.mark.anyio
async def test_already_published_alias_confirms_despite_later_acl_and_source_changes(monkeypatch):
    setup(monkeypatch, count=1)
    monkeypatch.setattr(qdrant_store, 'alias_target', lambda *a, **k: answer('new'))
    result = await reindex.swap('new', 'old', 'old', '0' * 64, False)
    assert result['active_collection'] == 'new'


@pytest.mark.anyio
async def test_builder_lock_does_not_block_document_storage_operations(monkeypatch):
    setup(monkeypatch, count=1)
    from app import storage_writes
    async with storage_writes.rebuild_guard('new', required=True):
        async with storage_writes.fenced():
            assert storage_writes._owner.get()
