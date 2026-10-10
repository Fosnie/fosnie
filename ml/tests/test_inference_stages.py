# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from types import SimpleNamespace

import httpx
import pytest
from app import embeddings, verify, reindex, ingest, rag_ctx, http_client
from app.inference_target import with_report, InferenceEnvelope
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context, anyio_backend, target
from test_inference_fixed import fixed


def stages(primary="embed",secondary="embed_desired"):
    value=fixed(primary)
    second=fixed(secondary)
    second["fixed_assignments"][secondary].update(model="second-model",api_key="second-key")
    value["fixed_assignments"].update(second["fixed_assignments"])
    value["fixed_budget"]["allowances"].update(second["fixed_budget"]["allowances"])
    if primary=="embed":
        value["fixed_assignments"][primary].update(active_index_revision=7,embedding_dimension=2,embedding_collection="old-collection")
    return value


def respond(request):
    body=json.loads(request.content)
    if request.url.path.endswith("embeddings"):
        return httpx.Response(200,json={"data":[{"index":0,"embedding":[1.0,2.0]}]})
    return httpx.Response(200,json={"model":body["model"],"spans":[],"verdicts":[{"verdict":"supported","score":1}]})


@pytest.mark.anyio
async def test_active_desired_concurrency_keeps_separate_keys_models_slots_and_revisions(monkeypatch):
    seen=[]
    async def reply(request): seen.append(request); await asyncio.sleep(0); return respond(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":stages()})
        await asyncio.gather(embeddings.embed(["active"]),embeddings.embed_with(["desired"],"http://fixed.invalid","second-model","second-key"))
        report=with_report({})["fixed_routing_report"]
        records={c["role"]:c for c in report["calls"]}
        assert records["embed"]["active_index_revision"]==7 and records["embed"]["desired_index_revision"] is None
        assert records["embed_desired"]["desired_index_revision"]==8 and records["embed_desired"]["active_index_revision"] is None
        assert {json.loads(r.content)["model"]:r.headers.get("authorization") for r in seen}=={"fixed-model":None,"second-model":"Bearer second-key"}
        with pytest.raises(BudgetExhausted): await embeddings.embed_desired(["late"])
        assert rag_ctx.cfg("_routing_fixed_target",None) is None


@pytest.mark.anyio
async def test_live_and_claims_verification_use_independent_models(monkeypatch):
    seen=[]
    async def reply(request): seen.append(request); await asyncio.sleep(0); return respond(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":stages("verify","verify_claims")})
        await asyncio.gather(verify.verify_live("c","q","a"),verify.verify_claims([{"text":"c","evidence":"e"}]))
        assert {r.url.path:json.loads(r.content)["routing_model"] for r in seen}=={"/v1/verify":"fixed-model","/v1/verify-claims":"second-model"}
        assert {c["role"] for c in with_report({})["fixed_routing_report"]["calls"]}=={"verify","verify_claims"}
        rag_ctx.set_overrides({"routing":fixed("verify")})
        with pytest.raises(ValueError,match="verify_claims"): await verify.verify_claims([{"text":"c"}])
    assert len(seen)==2


@pytest.mark.anyio
@pytest.mark.parametrize("base,model,key",[(None,"second-model","second-key"),("http://wrong","second-model","second-key"),
    ("http://fixed.invalid","wrong","second-key"),("http://fixed.invalid","second-model","")])
async def test_desired_explicit_fields_cannot_inherit_or_override_assignment(monkeypatch,base,model,key):
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("conflicting desired config reached HTTP"))
    rag_ctx.set_overrides({"routing":stages()})
    with pytest.raises(ValueError): await embeddings.embed_with(["x"],base,model,key)


def test_index_source_revision_cannot_be_relabelled():
    value=stages()
    value["fixed_assignments"]["embed_desired"]["active_index_revision"]=7
    with pytest.raises(ValueError): InferenceEnvelope.model_validate(value)


def reindex_stubs(monkeypatch, tmp_path):
    from app.config import settings
    monkeypatch.setattr(settings,"storage_root",str(tmp_path))
    mutations=[]
    async def alias(_, **kwargs): return "old-collection"
    async def create(name,dim): mutations.append(("create",name,dim))
    async def count(_): return 1
    async def scroll(_): yield [SimpleNamespace(id="row",payload={"chunk_text":"hello","knowledge_base_id":"kb","doc_id":"doc"})]
    async def upsert(name,rows,dense,sparse): mutations.append(("upsert",name,dense))
    monkeypatch.setattr(reindex.qdrant_store,"alias_target",alias)
    monkeypatch.setattr(reindex.qdrant_store,"create_kb_collection",create)
    monkeypatch.setattr(reindex.qdrant_store,"count",count)
    monkeypatch.setattr(reindex.qdrant_store,"scroll_all",scroll)
    monkeypatch.setattr(reindex.qdrant_store,"upsert_named",upsert)
    monkeypatch.setattr(reindex.sparse,"sparse_embed",lambda _:[])
    monkeypatch.setattr(reindex.qdrant_store,"swap_alias",lambda *a:pytest.fail("build must not promote an index"))
    class Client:
        async def get_aliases(self): return SimpleNamespace(aliases=[])
        async def collection_exists(self,name): return False
    monkeypatch.setattr(reindex.qdrant_store,"client",lambda:Client())
    return mutations


@pytest.mark.anyio
async def test_real_reindex_endpoint_uses_pinned_desired_collection_and_reports(monkeypatch,tmp_path):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    mutations=reindex_stubs(monkeypatch,tmp_path)
    seen=[]
    def reply(request): seen.append(request); return respond(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            response=await client.post("/reindex-embeddings",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json={
                "new_model":"second-model","new_dim":2,"source_kbs":["kb"],"source_docs":["doc"],"overrides":{"routing":stages()}})
    assert response.status_code==200,response.text
    events=[json.loads(line) for line in response.text.splitlines()]
    assert events[-1]["type"]=="built",events
    report=events[-1]["fixed_routing_report"]
    assert report["complete"] and report["calls"][0]["role"]=="embed_desired"
    assert mutations[0]==("create","new-collection",2) and mutations[1][1]=="new-collection"
    assert len(seen)==1 and json.loads(seen[0].content)["model"]=="second-model"


@pytest.mark.anyio
@pytest.mark.parametrize("mismatch",["model","dimension","source","same_collection","missing_desired"])
async def test_reindex_invalid_provenance_denied_before_collection_mutation(monkeypatch,mismatch,tmp_path):
    mutations=reindex_stubs(monkeypatch,tmp_path)
    value=stages(); dim=2; model="second-model"
    if mismatch=="model": model="wrong"
    if mismatch=="dimension": dim=3
    if mismatch=="source": value["fixed_assignments"]["embed"]["embedding_collection"]="wrong"
    if mismatch=="same_collection": value["fixed_assignments"]["embed_desired"]["embedding_collection"]="old-collection"
    if mismatch=="missing_desired": value=fixed()
    rag_ctx.set_overrides({"routing":value})
    with pytest.raises(ValueError): [event async for event in reindex.reindex_stream(dim,model,["kb"],["doc"])]
    assert mutations==[]


@pytest.mark.anyio
async def test_dual_write_conflict_fails_before_ocr_or_index_changes(monkeypatch):
    monkeypatch.setattr(ingest.extract,"extract_pages_ocr",lambda *a:pytest.fail("must preflight dual-write before extracting"))
    rag_ctx.set_overrides({"routing":stages()})
    with pytest.raises(ValueError,match="dual-write"):
        await ingest.ingest_document("doc","kb","file","text/plain",2,dual={"model":"wrong","dim":2})


@pytest.mark.anyio
@pytest.mark.parametrize("move_alias",[False,True])
async def test_dual_write_rechecks_alias_before_mutation_and_uses_pinned_collections(monkeypatch,move_alias):
    writes=[]; alias={"current":"old-collection"}
    async def alias_target(*args,**kwargs): return alias["current"]
    async def pages(*args): return [(1,"hello")]
    async def date(*args): return None
    async def delete(*args,**kwargs): writes.append(("delete",kwargs["collection"]))
    async def create(name,dim): writes.append(("create",name))
    async def upsert(name,rows,dense,sparse): writes.append(("upsert",name,dense))
    monkeypatch.setattr(ingest.qdrant_store,"alias_target",alias_target)
    monkeypatch.setattr(ingest.qdrant_store,"delete_doc",delete)
    monkeypatch.setattr(ingest.qdrant_store,"create_kb_collection",create)
    monkeypatch.setattr(ingest.qdrant_store,"upsert_named",upsert)
    monkeypatch.setattr(ingest.qdrant_store,"upsert",lambda *args:pytest.fail("strict writes must not follow a moving alias"))
    monkeypatch.setattr(ingest.extract,"extract_pages_ocr",pages)
    monkeypatch.setattr(ingest.metadata,"extract_effective_date",date)
    monkeypatch.setattr(ingest.chunker,"chunk_pages",lambda _: [{"text":"hello","page_number":1,"clause_section_ref":None}])
    monkeypatch.setattr(ingest.chunker,"toc_header",lambda _:None)
    monkeypatch.setattr(ingest.settings,"contextual_retrieval",False)
    monkeypatch.setattr(ingest.sparse,"sparse_embed",lambda _: [{"indices":[],"values":[]}])
    def reply(request):
        model=json.loads(request.content)["model"]
        if move_alias: alias["current"]="new-collection"
        return httpx.Response(200,json={"data":[{"index":0,"embedding":[1.0,1.0] if model=="fixed-model" else [2.0,2.0]}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        value=stages()
        value["assignments"]={"utility_summary":target(name="stage-summary")}
        value["budget"]={"id":value["decision_id"],"timeout_ms":5000,
            "allowances":{"utility_summary":{"max_calls":1,"input_tokens":16384,"output_tokens":512}}}
        rag_ctx.set_overrides({"routing":value,"parent_child":False})
        operation=ingest.ingest_document("doc","kb","file","text/plain",2,
            dual={"model":"second-model","dim":2,"base_url":"http://fixed.invalid","api_key":"second-key"})
        if move_alias:
            with pytest.raises(ValueError,match="alias changed"): await operation
            assert writes==[],"alias drift after embedding cannot replace either published corpus"
        else:
            result=await operation
            assert result["chunks"]==1
            assert writes==[("create","new-collection"),("delete","old-collection"),("delete","new-collection"),
                           ("upsert","old-collection",[[1.0,1.0]]),("upsert","new-collection",[[2.0,2.0]])]
    assert {c["role"] for c in with_report({})["fixed_routing_report"]["calls"]}=={"embed","embed_desired"}


@pytest.mark.anyio
async def test_strict_alias_failure_is_not_a_guessed_source_and_named_delete_ignores_alias(monkeypatch):
    from app import qdrant_store
    deleted=[]
    async def aliases(): raise RuntimeError("Qdrant unavailable")
    async def exists(_): return True
    async def delete(name,**kwargs): deleted.append(name)
    monkeypatch.setattr(qdrant_store,"client",lambda:SimpleNamespace(get_aliases=aliases,collection_exists=exists,delete=delete))
    with pytest.raises(RuntimeError): await qdrant_store.alias_target("logical",strict=True)
    assert await qdrant_store.alias_target("logical")=="logical", "legacy fallback remains"
    await qdrant_store.delete_doc("kb","doc",collection="physical-old")
    assert deleted[0]=="physical-old" and qdrant_store.COLLECTION not in deleted
