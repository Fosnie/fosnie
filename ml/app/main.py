# Copyright 2026 Private AI Ltd (SC881079)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""pai-ml — generation-only slice. The platform's LLM client; Rust composes the
seven-layer prompt and streams generation through here (it never calls the LLM
directly — topology). RAG, extraction, etc. land in later slices."""

import asyncio
import hmac
import importlib
import json
import pkgutil
import logging
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

import httpx
from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse
from prometheus_client import CONTENT_TYPE_LATEST, Counter, Histogram, generate_latest
from pydantic import BaseModel

from . import http_client
from . import ingest as ingest_mod
from . import llm
from . import provider_test as provider_test_mod
from . import qdrant_store
from . import retrieve as retrieve_mod
from .config import settings
from .paths import safe_path


def _setup_logging() -> None:
    """Persist INFO+ logs to a rotating file. uvicorn ships
    console-only, so a diagnostic run (whose `rag turn …` summary is INFO) is lost when
    the process restarts. Attach a RotatingFileHandler to the
    ROOT logger so every `pai*`/`pai-ml*` logger propagates to it; the console stays.
    Idempotent (guarded so uvicorn's reloader can't stack handlers). Empty log_dir or
    an unwritable path disables the file sink rather than crashing boot."""
    if not settings.log_dir:
        return
    root = logging.getLogger()
    marker = "pai-ml-file"
    if any(getattr(h, "_pai_marker", None) == marker for h in root.handlers):
        return
    try:
        log_path = Path(settings.log_dir)
        log_path.mkdir(parents=True, exist_ok=True)
        from logging.handlers import RotatingFileHandler

        handler = RotatingFileHandler(
            log_path / "pai-ml.log",
            maxBytes=settings.log_max_bytes,
            backupCount=settings.log_backups,
            encoding="utf-8",
        )
        handler._pai_marker = marker  # type: ignore[attr-defined]
        handler.setLevel(logging.INFO)
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
        root.addHandler(handler)
        if root.level > logging.INFO or root.level == logging.NOTSET:
            root.setLevel(logging.INFO)  # ensure INFO records reach the handler
        logging.getLogger("pai-ml.main").info("file logging → %s", log_path / "pai-ml.log")
    except OSError as e:
        logging.getLogger("pai-ml.main").warning("file logging disabled (%s)", e)


_setup_logging()


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Own the process-wide shared httpx client: create
    one pooled client at startup, close it on shutdown."""
    client = http_client.build_client()
    http_client.set_client(client)
    try:
        yield
    finally:
        await client.aclose()
        http_client.set_client(None)


app = FastAPI(title="pai-ml", version="0.1.0", lifespan=lifespan)

_ML_KEY_HEADER = "x-pai-ml-key"


@app.middleware("http")
async def guard(request: Request, call_next):
    """Reject calls lacking the shared secret (when configured) and cap body
    size. Only `/health` is open (liveness probes need no secret). `/metrics` is
    gated too — fail-closed, so system telemetry is not readable by anything that
    cannot present the secret. The Rust backend authenticates with the
    `X-PAI-ML-Key` header; a Prometheus scraper (which cannot send arbitrary
    headers) uses `Authorization: Bearer <secret>` — both are accepted."""
    if request.url.path != "/health":
        secret = settings.shared_secret
        if secret:
            presented = request.headers.get(_ML_KEY_HEADER, "")
            if not presented:
                authz = request.headers.get("authorization", "")
                if authz.startswith("Bearer "):
                    presented = authz[7:]
            if not hmac.compare_digest(presented, secret):
                return JSONResponse({"detail": "unauthorised"}, status_code=401)
        clen = request.headers.get("content-length")
        if clen is not None:
            try:
                if int(clen) > settings.max_request_bytes:
                    return JSONResponse({"detail": "request too large"}, status_code=413)
            except ValueError:
                return JSONResponse({"detail": "bad content-length"}, status_code=400)
    return await call_next(request)


# --- Prometheus metrics -------------------------------------------------------
# ML routes are body-based (no path-id params), so the raw path stays low-cardinality.
_ML_LATENCY = Histogram("ml_request_seconds", "ML request latency by endpoint", ["path", "method"])
_ML_REQUESTS = Counter("ml_requests_total", "ML requests by endpoint + status", ["path", "method", "status"])


@app.middleware("http")
async def metrics_timer(request: Request, call_next):
    import time

    start = time.perf_counter()
    response = await call_next(request)
    path, method = request.url.path, request.method
    _ML_LATENCY.labels(path=path, method=method).observe(time.perf_counter() - start)
    _ML_REQUESTS.labels(path=path, method=method, status=str(response.status_code)).inc()
    return response


@app.get("/metrics")
async def metrics() -> Response:
    """Prometheus exposition. Gated by the shared secret in `guard` (above) —
    :8090 is internal-only AND now fail-closed; a scraper presents the secret as
    `Authorization: Bearer <secret>`."""
    return Response(content=generate_latest(), media_type=CONTENT_TYPE_LATEST)


class Sampling(BaseModel):
    temperature: float | None = None
    top_p: float | None = None
    max_tokens: int | None = None
    frequency_penalty: float | None = None
    presence_penalty: float | None = None
    # Reasoning-effort hint for thinking-capable providers (OpenAI gpt-5.x/o-series,
    # Gemini 2.5). Sent only by utility calls (e.g. chat-title naming); llm.py clamps
    # it per provider and omits it where unsupported.
    reasoning_effort: str | None = None


class Message(BaseModel):
    # `content` is a plain string for ordinary turns, or an OpenAI-shape list of
    # content parts (`{type:"text",…}` / `{type:"image_url",…}`) for multimodal
    # input. The OpenAI/LiteLLM path forwards parts natively; the native
    # Anthropic/Gemini adapters translate them (see *_adapter.py).
    role: str
    content: str | list[dict[str, Any]]


class GenerateRequest(BaseModel):
    # Raw OpenAI-shape message dicts (not the strict `Message` model) so an
    # assistant `tool_calls` array and a `role:"tool"` result carry through intact
    # — the streaming answer can now request further retrieval mid-flight, and the
    # continuation request replays that tool history. Same shape `/chat-step` uses.
    messages: list[dict[str, Any]]
    sampling: Sampling = Sampling()
    model: str | None = None
    # Optional tool schemas. Present only when the answering model is allowed to
    # call a tool (currently just the library top-up) while streaming; absent ⇒ a
    # plain answer stream, byte-identical to before.
    tools: list[dict[str, Any]] | None = None
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.get("/health")
async def health() -> dict[str, str]:
    return {"status": "ok"}


async def _resolve_model() -> tuple[str, int]:
    """Learn the served model id + context window: vLLM advertises max_model_len
    via /v1/models; Ollama does not, so fall back to the configured value."""
    model_id = settings.llm_model
    max_len = settings.llm_max_model_len
    try:
        client = http_client.get_client()
        r = await client.get(
            f"{settings.llm_base_url.rstrip('/')}/models",
            headers={"Authorization": f"Bearer {settings.llm_api_key}"},
            timeout=10.0,
        )
        if r.status_code == 200:
            data = r.json().get("data", [])
            ids = [m.get("id") for m in data]
            if settings.llm_model not in ids and ids:
                model_id = ids[0]
            for m in data:
                if m.get("id") == model_id and m.get("max_model_len"):
                    max_len = m["max_model_len"]
    except Exception:
        pass  # best-effort; fall back to configured values
    return model_id, max_len


@app.get("/model-info")
async def model_info() -> dict[str, Any]:
    """Learn the served model + context window (chat-turn token budgeting)."""
    model_id, max_len = await _resolve_model()
    return {"model_id": model_id, "max_model_len": max_len}


@app.post("/generate")
async def generate(req: GenerateRequest) -> StreamingResponse:
    from . import rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    messages = req.messages
    sampling = req.sampling.model_dump(exclude_none=True)

    async def ndjson():
        try:
            llm.set_stage("generate")
            async for event in llm.stream_chat(messages, sampling, req.model, tools=req.tools):
                yield json.dumps(event) + "\n"
        except Exception as e:  # surface upstream failures as a terminal event
            yield json.dumps({"type": "error", "message": str(e)}) + "\n"

    return StreamingResponse(ndjson(), media_type="application/x-ndjson")


# --- RAG ---------------------------------------------------------------------


class EnsureCollectionRequest(BaseModel):
    dimension: int


class IngestRequest(BaseModel):
    doc_id: str
    kb_id: str
    path: str
    mime: str | None = None
    dimension: int
    # Optional super-admin chunking overrides (else the service's own defaults).
    chunk_size: int | None = None
    chunk_overlap: int | None = None
    pdfplumber: bool | None = None
    # Per-KB parent–child chunking; None ⇒ the service default.
    parent_child: bool | None = None
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None
    # Dual-write target during a blue-green re-index: also
    # embed with this NEW model and upsert into the rebuilt collection. {dim, model,
    # base_url, api_key} or None outside the migration window.
    dual: dict | None = None


class DeleteDocRequest(BaseModel):
    kb_id: str
    doc_id: str


class RetrieveRequest(BaseModel):
    prompt: str
    kb_ids: list[str]
    # Source-ACL retrieval deny-list: `doc_id`s the caller is
    # not entitled to under an `enforce` connector mapping. Default empty ⇒ no
    # filtering ⇒ byte-identical to before the feature.
    deny_doc_ids: list[str] = []
    # Optional super-admin runtime overrides (else the service's own defaults).
    top_k: int | None = None
    over_retrieval: int | None = None
    max_rounds: int | None = None
    query_variants: int | None = None
    rerank_enabled: bool | None = None
    grade_skip_threshold: float | None = None
    max_subqueries: int | None = None
    max_parents: int | None = None
    max_context_chunks: int | None = None
    pool_uncited_per_subq: int | None = None
    pool_per_subq_budget: int | None = None
    pool_hard_cap: int | None = None
    pool_crossref_reserve: int | None = None
    # Deterministic retrieval expansion.
    anchor_lookup_max: int | None = None
    neighbor_span: int | None = None
    crossref_max_sections: int | None = None
    toc_max_sections: int | None = None
    late_anchor_cap: int | None = None
    gap_round_enabled: bool | None = None
    gap_rounds: int | None = None
    gap_reserve: int | None = None
    gap_deadline_secs: int | None = None
    gap_diminishing_unseen: float | None = None
    gap_escalate: bool | None = None
    # NDJSON progress streaming (A4): live retrieval activity ending in a
    # `{"type":"done", context, citations}` line. Off ⇒ today's single dict.
    stream: bool = False
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/collections")
async def ensure_collection(req: EnsureCollectionRequest) -> dict:
    await qdrant_store.ensure_collection(req.dimension)
    return {"ok": True, "collection": qdrant_store.COLLECTION}


@app.post("/ingest")
async def ingest(req: IngestRequest) -> dict:
    from . import rag_ctx
    from .ocr import OcrUnavailable

    rag_ctx.set_overrides({"chunk_size": req.chunk_size, "chunk_overlap": req.chunk_overlap, "pdfplumber": req.pdfplumber, "parent_child": req.parent_child, **(req.overrides or {})})
    try:
        return await ingest_mod.ingest_document(
            req.doc_id, req.kb_id, safe_path(req.path), req.mime, req.dimension, dual=req.dual
        )
    except OcrUnavailable as e:
        # A scanned PDF / image that could not be OCR'd. Surface a clear 422 so
        # the backend marks the document `error` (never index empty content).
        raise HTTPException(status_code=422, detail=f"OCR required but unavailable: {e}") from e


@app.post("/delete-doc")
async def delete_doc(req: DeleteDocRequest) -> dict:
    await qdrant_store.delete_doc(req.kb_id, req.doc_id)
    return {"ok": True}


@app.post("/retrieve")
async def retrieve(req: RetrieveRequest) -> dict:
    from . import rag_ctx

    rag_ctx.set_overrides({
        "top_k": req.top_k,
        "over_retrieval": req.over_retrieval,
        "max_rounds": req.max_rounds,
        "query_variants": req.query_variants,
        "rerank_enabled": req.rerank_enabled,
        "grade_skip_threshold": req.grade_skip_threshold,
        "max_subqueries": req.max_subqueries,
        "max_parents": req.max_parents,
        "max_context_chunks": req.max_context_chunks,
        "pool_uncited_per_subq": req.pool_uncited_per_subq,
        "pool_per_subq_budget": req.pool_per_subq_budget,
        "pool_hard_cap": req.pool_hard_cap,
        "pool_crossref_reserve": req.pool_crossref_reserve,
        "anchor_lookup_max": req.anchor_lookup_max,
        "neighbor_span": req.neighbor_span,
        "crossref_max_sections": req.crossref_max_sections,
        "toc_max_sections": req.toc_max_sections,
        "late_anchor_cap": req.late_anchor_cap,
        "gap_round_enabled": req.gap_round_enabled,
        "gap_rounds": req.gap_rounds,
        "gap_reserve": req.gap_reserve,
        "gap_deadline_secs": req.gap_deadline_secs,
        "gap_diminishing_unseen": req.gap_diminishing_unseen,
        "gap_escalate": req.gap_escalate,
        **(req.overrides or {}),
    })
    if req.stream:
        from . import retrieve_stream

        async def lines():
            async for event in retrieve_stream.stream_events(req.prompt, req.kb_ids, req.deny_doc_ids):
                yield json.dumps(event) + "\n"

        return StreamingResponse(lines(), media_type="application/x-ndjson")
    return await retrieve_mod.retrieve(req.prompt, req.kb_ids, req.deny_doc_ids)


class WebSearchRequest(BaseModel):
    query: str
    recency: str | None = None  # any | year | month | week | day
    depth: str | None = None    # quick | standard | deep
    # Runtime/admin + per-Agent overrides (None ⇒ the service's env defaults).
    # A present-but-empty list string means "list off" — it still overrides env.
    domain_allowlist: str | None = None
    domain_blocklist: str | None = None
    allowlist_only: bool | None = None
    robots_policy: str | None = None  # user_triggered | respect
    max_fetches: int | None = None    # per-Agent min-clamp on the fetch budget
    # NDJSON progress streaming.
    stream: bool = False
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/web_search")
async def web_search(req: WebSearchRequest):
    """Web-search pipeline: SERP via
    the configured provider → SSRF-guarded paced fetch → extract → rerank →
    digest + web citations. The Rust backend only calls this after its egress
    gate passed — the dormant/enabled decision is NOT made here. With
    `stream=true` the response is NDJSON progress events ending in a
    `{"type":"done", digest, citations}` line."""
    from . import rag_ctx
    from .web import pipeline as web_pipeline

    rag_ctx.set_overrides({
        "web_domain_allowlist": req.domain_allowlist,
        "web_domain_blocklist": req.domain_blocklist,
        "web_allowlist_only": req.allowlist_only,
        "web_robots_policy": req.robots_policy,
        "web_max_fetches": req.max_fetches,
        **(req.overrides or {}),
    })
    if req.stream:
        from .web import stream as web_stream

        async def lines():
            async for event in web_stream.stream_events(
                req.query, recency=req.recency or "any", depth=req.depth or "standard"
            ):
                yield json.dumps(event) + "\n"

        return StreamingResponse(lines(), media_type="application/x-ndjson")
    return await web_pipeline.web_search(
        req.query, recency=req.recency or "any", depth=req.depth or "standard"
    )


class ResearchDoc(BaseModel):
    doc_id: str
    kb_id: str
    kb_name: str | None = None
    path: str
    mime: str | None = None
    filename: str | None = None


class DeepResearchRequest(BaseModel):
    question: str
    template: str | None = None  # exploration | formal | freeform | literature
    # Corpus census / hybrid (Phase 2). source ∈ {web, files, hybrid}. For files
    # and hybrid the Rust backend resolves the readable scope and sends the doc
    # inventory; ML reads the files (paths are storage-confined by safe_path).
    source: str | None = None
    kb_ids: list[str] = []
    docs: list[ResearchDoc] = []
    total_docs: int | None = None  # true corpus size (for honest coverage)
    refinements: list[str] = []    # triage-chip answers steering scope/voice
    # Citation verification + ground-or-cut. The Rust backend computes
    # `verify = features.groundedness && research.verify` and sends it; ML never
    # reads Rust config. Off ⇒ Phases 1-2 behaviour unchanged.
    verify: bool = False
    # Runtime research budget overrides (super-admin knobs), applied as rag_ctx
    # overrides for this run (None ⇒ ML env defaults).
    research_max_minutes: float | None = None
    research_census_cap: int | None = None
    research_notes_concurrency: int | None = None
    # The same runtime/admin web overrides /web_search accepts — web/hybrid
    # collection rides the web loop, so the same policy applies. (Ignored by a
    # files-only run, which performs zero egress.)
    domain_allowlist: str | None = None
    domain_blocklist: str | None = None
    allowlist_only: bool | None = None
    robots_policy: str | None = None
    max_fetches: int | None = None
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/deep_research")
async def deep_research(req: DeepResearchRequest):
    """Deep Research: collect evidence (web loop,
    corpus census, or both), synthesise a structured cited report (memory bank →
    outline → per-section writer → coherence → checks). Always streams NDJSON:
    progress events, then `{"type": "done", title, report_md, citations,
    doc_citations}`. The Rust backend only calls this after its egress gate
    passed (web/hybrid); a files-only run performs no egress."""
    from . import rag_ctx
    from .config import settings as cfg_settings
    from .paths import safe_path
    from .research import stream as research_stream

    rag_ctx.set_overrides({
        "web_domain_allowlist": req.domain_allowlist,
        "web_domain_blocklist": req.domain_blocklist,
        "web_allowlist_only": req.allowlist_only,
        "web_robots_policy": req.robots_policy,
        "web_max_fetches": req.max_fetches,
        # Runtime research budget knobs (read via cfg() in pipeline/budgets).
        "research_max_minutes": req.research_max_minutes,
        "research_census_cap": req.research_census_cap,
        "research_notes_concurrency": req.research_notes_concurrency,
        **(req.overrides or {}),
    })

    docs = [
        {
            "doc_id": d.doc_id,
            "kb_id": d.kb_id,
            "kb_name": d.kb_name or "",
            "path": safe_path(d.path),
            "mime": d.mime,
            "filename": d.filename or d.doc_id,
        }
        for d in req.docs
    ]

    async def lines():
        async for event in research_stream.stream_events(
            req.question,
            template=req.template or "exploration",
            source=req.source or "web",
            kb_ids=req.kb_ids,
            docs=docs,
            total_docs=req.total_docs,
            refinements=req.refinements,
            verify=req.verify,
        ):
            yield json.dumps(event) + "\n"

    return StreamingResponse(lines(), media_type="application/x-ndjson")


class ResearchTriageRequest(BaseModel):
    question: str
    source: str | None = None
    scope: list[dict] = []  # [{index, name, kind, doc_count}]


@app.post("/research/triage")
async def research_triage(req: ResearchTriageRequest) -> dict:
    """Ambiguity triage for the plan gate: one cheap
    LLM call deciding whether the question needs ≤3 quick clarifying chips given
    the visible scope. Side-effect-free; degrades to no questions on any
    failure. The Rust backend maps the returned scope indices → kb_ids."""
    from .research import triage as triage_mod

    return await triage_mod.triage(req.question, req.source or "web", req.scope)


class VerifyRequest(BaseModel):
    context: str
    question: str
    answer: str
    model: str | None = None
    strictness: str | None = None  # strict | lenient (admin dial)
    threshold: float | None = None  # min flag confidence
    hhem_filter: bool | None = None  # HHEM second opinion
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/verify")
async def verify(req: VerifyRequest) -> dict:
    """Mode A (live) groundedness: flag spans of `answer` unsupported by `context`.
    Off the hot path — the Rust backend
    calls this from a spawned task post-stream, never inline with generation.
    Returns {spans, score, total, flagged, model}; fails open when the verifier
    is disabled/unreachable."""
    from . import rag_ctx
    from . import verify as verify_mod

    rag_ctx.set_overrides(req.overrides or {})

    return await verify_mod.verify_live(
        req.context, req.question, req.answer, req.model,
        strictness=req.strictness or "strict",
        threshold=req.threshold or 0.0,
        hhem_filter=bool(req.hhem_filter),
    )


class EmbedDimensionRequest(BaseModel):
    # Provider overrides ({embed_base_url/_model/_api_key}); empty ⇒ .env defaults.
    overrides: dict | None = None


@app.post("/embed-dimension")
async def embed_dimension(req: EmbedDimensionRequest | None = None) -> dict:
    """Probe the configured embedding model's id + dimension. Accepts the same
    provider-override map as /retrieve so a deployment that points `embed` at a
    cloud API (DB provider config) is honoured here too — not just the .env default."""
    from . import embeddings, rag_ctx

    rag_ctx.set_overrides((req.overrides if req else None) or {})
    model = rag_ctx.cfg("embed_model", settings.embed_model)
    return {"model": model, "dimension": await embeddings.dimension()}


class ReindexRequest(BaseModel):
    # The NEW (desired) embed config to build the new index with.
    new_dim: int
    new_model: str
    new_base_url: str | None = None
    new_api_key: str | None = None


@app.post("/reindex-embeddings")
async def reindex_embeddings(req: ReindexRequest) -> StreamingResponse:
    """Build the blue-green re-index target. Streams NDJSON
    progress; does NOT swap the alias (the backend calls /swap-embedding-alias once
    the build is verified, so the swap + provenance promotion are adjacent)."""
    from . import rag_ctx, reindex

    async def ndjson():
        # The new model drives `embeddings.embed` during the build.
        rag_ctx.set_overrides({
            "embed_base_url": req.new_base_url,
            "embed_model": req.new_model,
            "embed_api_key": req.new_api_key,
        })
        try:
            async for ev in reindex.reindex_stream(req.new_dim, req.new_model):
                yield json.dumps(ev) + "\n"
        except Exception as e:  # surface as a terminal event; backend marks failed
            yield json.dumps({"type": "error", "message": str(e)}) + "\n"

    return StreamingResponse(ndjson(), media_type="application/x-ndjson")


class SwapAliasRequest(BaseModel):
    new_collection: str
    old_collection: str | None = None


@app.post("/swap-embedding-alias")
async def swap_embedding_alias(req: SwapAliasRequest) -> dict:
    """Atomically point the `pai_kb` alias at the rebuilt collection + drop the
    superseded one (step 5)."""
    from . import reindex

    return await reindex.swap(req.new_collection, req.old_collection)


class ProviderTestRequest(BaseModel):
    role: str  # llm | embed | rerank | ocr | stt | tts | verify
    # The provider config to test, as a {role}_base_url/_model/_api_key override
    # map. The Rust backend resolves + decrypts the key and
    # passes it here; the key is never persisted or echoed back.
    overrides: dict | None = None


@app.post("/provider/test")
async def provider_test(req: ProviderTestRequest) -> dict:
    """Provider health probe: a minimal real call for the
    given role with the supplied config, returning {ok, latency_ms, error?,
    detail?, model?}. Readable errors, never the api_key."""
    from . import rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    return await provider_test_mod.probe(req.role)


# --- Tool-call loop support --------------------------------------------------


class ChatStepRequest(BaseModel):
    messages: list[dict[str, Any]]
    tools: list[dict[str, Any]] | None = None
    sampling: Sampling = Sampling()
    model: str | None = None
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/chat-step")
async def chat_step(req: ChatStepRequest) -> dict[str, Any]:
    from . import rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    sampling = req.sampling.model_dump(exclude_none=True)
    llm.set_stage("tool_loop")
    # A non-streaming reasoning step can run for minutes; surface a timeout / upstream
    # failure as a clean status instead of a raw 500 after a long wait.
    try:
        return await llm.chat_step(req.messages, req.tools, sampling, req.model)
    except httpx.TimeoutException as e:
        logging.getLogger("pai-ml.main").warning("chat-step timed out: %s", e)
        raise HTTPException(status_code=504, detail="reasoning timed out") from e
    except httpx.HTTPError as e:
        logging.getLogger("pai-ml.main").warning("chat-step upstream error: %s", e)
        raise HTTPException(status_code=502, detail="LLM upstream error") from e


class ReadDocumentRequest(BaseModel):
    path: str
    mime: str | None = None
    # The task to read the document FOR. When given and the document is too large
    # to stuff, the read is an exhaustive map-reduce focused on this task.
    prompt: str | None = None
    # Hard ceiling on returned characters (a final safety cap). 0 = no extra cap.
    max_chars: int = 0
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


def _est_tokens(s: str) -> int:
    return len(s) // 4  # chars/4, matching the Rust budgeter


@app.post("/read-document")
async def read_document(req: ReadDocumentRequest) -> dict[str, Any]:
    """Three whole-document modes (.md). Deterministic
    token routing on the *exact* extracted size: STUFF the whole document when it
    fits a safe fraction of the context window; otherwise (and given a task to
    focus on) run an EXHAUSTIVE map-reduce. With no task, a too-large document is
    stuffed and truncated to the budget rather than guessed at."""
    from . import extract, map_reduce, rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    sp = safe_path(req.path)
    is_image = (req.mime or "").startswith("image/") or Path(sp).suffix.lower() in extract._IMAGE_SUFFIXES
    if is_image:
        # Images have no native text layer — OCR them. The sync extract() has no
        # image branch and would read the raw bytes as UTF-8, yielding binary
        # garbage. This is the text/fallback path for non-vision models; vision
        # models receive the image itself (built in chat::run_turn).
        pages = await extract.extract_pages_ocr(sp, req.mime)
        text = "\n".join(t for _, t in pages)
    else:
        # Offload blocking extraction (pypdf/openpyxl/lxml + file IO) so the event
        # loop keeps serving other requests.
        text = await asyncio.to_thread(extract.extract, sp, req.mime)
    _, max_model_len = await _resolve_model()
    stuff_budget_tokens = int(max_model_len * settings.stuff_fraction)
    fits = _est_tokens(text) <= stuff_budget_tokens

    if fits or not (req.prompt and req.prompt.strip()):
        # STUFF (whole document). If it doesn't fit and we have no task to map
        # against, cap to the stuff budget — never silently feed an over-long doc.
        cap = stuff_budget_tokens * 4 if not fits else (req.max_chars or len(text))
        out = text[:cap] if cap and len(text) > cap else text
        return {"mode": "stuff", "text": out, "truncated": len(out) < len(text)}

    # MAP-REDUCE (exhaustive, structured accumulation). Bounded: cap the text so an
    # agent pointed at a huge document can't trigger hundreds of LLM calls (minutes,
    # engine saturation) — past the cap the agent should rely on RAG retrieval.
    mr_truncated = len(text) > settings.read_document_max_chars
    capped = text[: settings.read_document_max_chars] if mr_truncated else text
    result = await map_reduce.map_reduce(capped, req.prompt.strip())
    digest = result["text"]
    if req.max_chars and len(digest) > req.max_chars:
        digest = digest[: req.max_chars]
    return {"mode": "map_reduce", "text": digest, "sections": len(result["sections"]), "truncated": mr_truncated}


class VerifyDraftRequest(BaseModel):
    path: str
    mime: str | None = None
    kb_ids: list[str] = []
    max_claims: int | None = None
    strictness: str | None = None  # strict | lenient (admin dial)
    threshold: float | None = None
    hhem_filter: bool | None = None  # HHEM second opinion
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/verify-draft")
async def verify_draft(req: VerifyDraftRequest) -> dict[str, Any]:
    """Mode B: decompose a draft into
    atomic claims, bind each to evidence retrieved from the caller's KBs, verify
    each claim, and score. Per-claim windowing → N bounded verifier calls
    regardless of document length (invariant 4). A throughput background job."""
    import asyncio

    from . import chunker, decompose, extract, locate, rag_ctx
    from . import retrieve as retrieve_mod
    from . import verify as verify_mod

    rag_ctx.set_overrides(req.overrides or {})
    empty = {"claims": [], "score": None, "total": 0,
             "supported": 0, "contradicted": 0, "not_mentioned": 0}
    text = await asyncio.to_thread(extract.extract, safe_path(req.path), req.mime)
    if not text.strip():
        return empty

    # 1) Decompose by section — the single biggest quality lever.
    sections = chunker.chunk_text(text, size=settings.map_window_chars)
    sec_offsets = locate.section_offsets(text, sections)
    dsem = asyncio.Semaphore(max(1, settings.map_concurrency))

    async def _decompose_section(idx: int, section: str) -> list[tuple[int, str]]:
        async with dsem:
            claims = await decompose.decompose_claims(section)
        return [(idx, c) for c in claims]

    grouped = await asyncio.gather(*[_decompose_section(i, s) for i, s in enumerate(sections)])
    claims = [pair for group in grouped for pair in group][: (req.max_claims or settings.verify_draft_max_claims)]
    if not claims:
        return empty

    # 2) Bind evidence — re-retrieve the top-k chunks for each claim from the
    # caller's KB allow-list (un-cited claims are flagged higher-risk).
    bsem = asyncio.Semaphore(max(1, settings.retrieve_concurrency))
    k = settings.verify_draft_evidence_k

    async def _bind(claim: str) -> str:
        if not req.kb_ids:
            return ""
        hits = await retrieve_mod._search_one(claim, req.kb_ids, bsem)
        return "\n\n".join(h["payload"]["chunk_text"] for h in hits[:k])

    evidences = await asyncio.gather(*[_bind(c) for _, c in claims])

    # 3) Verify each claim against its evidence (FactCG + NLI subtype, batched).
    pairs = [{"text": c, "evidence": ev} for (_, c), ev in zip(claims, evidences)]
    verdicts = await verify_mod.verify_claims(pairs, hhem_filter=bool(req.hhem_filter))

    # 4) Aggregate. Score = supported / total (RAGAS-strict).
    counts = {"supported": 0, "contradicted": 0, "not_mentioned": 0}
    out_claims = []
    for (sec_i, claim), ev, v in zip(claims, evidences, verdicts):
        verdict = v.get("verdict", "not_mentioned")
        counts[verdict] = counts.get(verdict, 0) + 1
        # Locate the (rephrased) claim back to its verbatim span in the document
        # — feeds the inline highlight and ground-or-cut repair (–4.6).
        hint = sec_offsets[sec_i] if 0 <= sec_i < len(sec_offsets) else 0
        out_claims.append({
            "text": claim,
            "verdict": verdict,
            "score": float(v.get("score", 0.0)),
            "evidence": ev[:600],
            "section": f"section {sec_i + 1}",
            "had_citation": bool(ev.strip()),
            "source_span": locate.locate(claim, text, hint),
        })
    total = len(out_claims)
    # Score per strictness: strict = supported/total (RAGAS-strict); lenient =
    # only a contradiction fails, so not-mentioned claims count as grounded too.
    grounded = counts["supported"]
    if (req.strictness or "strict") == "lenient":
        grounded += counts["not_mentioned"]
    return {
        "claims": out_claims,
        "score": (grounded / total) if total else None,
        "total": total,
        **counts,
    }


class RepairClaimIn(BaseModel):
    text: str
    source_text: str | None = None
    verdict: str = "not_mentioned"
    evidence: str | None = None
    score: float | None = None


class RepairDraftRequest(BaseModel):
    claims: list[RepairClaimIn] = []
    kb_ids: list[str] = []
    strictness: str | None = None


@app.post("/repair-draft")
async def repair_draft(req: RepairDraftRequest) -> dict[str, Any]:
    """Ground-or-cut repair: regenerate or cut each flagged claim, re-verify
    the new citation, and return per-claim actions for the backend to surface as
    tracked-change proposals. Consumes a finished run's flagged claims — it does not
    re-extract or re-decompose the document."""
    from . import repair as repair_mod

    claims = [c.model_dump() for c in req.claims]
    results = await repair_mod.repair_claims(claims, req.kb_ids, req.strictness or "strict")
    return {"results": results}


# --- Tracked changes (DOCX) --------------------------------------------------


class EditInput(BaseModel):
    find: str = ""
    replace: str = ""
    context_before: str | None = None
    context_after: str | None = None


class ApplyTrackedChangesRequest(BaseModel):
    path: str
    out_path: str
    edits: list[EditInput]
    author: str = "Assistant"


class ResolveTrackedChangeRequest(BaseModel):
    path: str
    out_path: str
    w_id: str
    action: str  # 'accept' | 'reject'


class ResolveAllRequest(BaseModel):
    path: str
    out_path: str
    action: str  # 'accept' | 'reject'
    author_filter: str | None = None


@app.post("/apply-tracked-changes")
async def apply_tracked_changes(req: ApplyTrackedChangesRequest) -> dict[str, Any]:
    from . import tracked_changes

    edits = [e.model_dump() for e in req.edits]
    # Offload blocking DOCX rewrite (lxml + zipfile) off the event loop (L7).
    return await asyncio.to_thread(
        tracked_changes.apply_tracked_changes,
        safe_path(req.path), safe_path(req.out_path), edits, req.author,
    )


@app.post("/resolve-tracked-change")
async def resolve_tracked_change(req: ResolveTrackedChangeRequest) -> dict[str, Any]:
    from . import tracked_changes

    return await asyncio.to_thread(
        tracked_changes.resolve_tracked_change,
        safe_path(req.path), safe_path(req.out_path), req.w_id, req.action,
    )


@app.post("/resolve-tracked-changes")
async def resolve_tracked_changes(req: ResolveAllRequest) -> dict[str, Any]:
    from . import tracked_changes

    return await asyncio.to_thread(
        tracked_changes.resolve_all,
        safe_path(req.path), safe_path(req.out_path), req.action, req.author_filter,
    )


# --- DOCX→PDF rendition ------------------------------------------------------


class RenderRequest(BaseModel):
    path: str
    out_dir: str


@app.get("/render/available")
async def render_available() -> dict[str, bool]:
    from . import render

    return {"available": render.available()}


@app.post("/render")
async def render(req: RenderRequest) -> dict[str, Any]:
    from . import render as render_mod

    if not render_mod.available():
        raise HTTPException(status_code=503, detail="rendition unavailable (LibreOffice absent)")
    try:
        # Offload the blocking LibreOffice subprocess off the event loop (L7).
        pdf_path = await asyncio.to_thread(
            render_mod.docx_to_pdf, safe_path(req.path), safe_path(req.out_dir)
        )
    except RuntimeError as e:
        raise HTTPException(status_code=503, detail=str(e)) from e
    return {"pdf_path": pdf_path}


# --- Tabular review ----------------------------------------------------------


class ReviewDocument(BaseModel):
    document_id: str
    path: str
    mime: str | None = None


class ReviewColumn(BaseModel):
    key: str
    format: str = "text"
    prompt: str
    mechanism: str = "stuff"  # stuff | per_document_rag | map_section


class GenerateReviewRequest(BaseModel):
    documents: list[ReviewDocument]
    columns: list[ReviewColumn]
    concurrency: int | None = None
    # Provider overrides: {role}_base_url/_model/_api_key.
    overrides: dict | None = None


@app.post("/generate-review")
async def generate_review(req: GenerateReviewRequest) -> StreamingResponse:
    from . import rag_ctx, tabular

    rag_ctx.set_overrides(req.overrides or {})
    documents = [{**d.model_dump(), "path": safe_path(d.path)} for d in req.documents]
    columns = [c.model_dump() for c in req.columns]

    async def ndjson():
        try:
            async for event in tabular.generate_review(documents, columns, req.concurrency):
                yield json.dumps(event) + "\n"
        except Exception as e:
            yield json.dumps({"type": "error", "message": str(e)}) + "\n"

    return StreamingResponse(ndjson(), media_type="application/x-ndjson")


class ExportColumn(BaseModel):
    key: str
    name: str


class ExportRow(BaseModel):
    document: str
    cells: dict[str, Any] = {}


class ExportReviewRequest(BaseModel):
    name: str
    columns: list[ExportColumn]
    rows: list[ExportRow]
    out_path: str


@app.post("/export-review")
async def export_review(req: ExportReviewRequest) -> dict[str, Any]:
    from . import tabular

    columns = [c.model_dump() for c in req.columns]
    rows = [r.model_dump() for r in req.rows]
    path = tabular.export_xlsx(req.name, columns, rows, safe_path(req.out_path))
    return {"path": path}


# --- Generated artefacts -----------------------------------------------------


class GenerateArtefactRequest(BaseModel):
    kind: str  # docx | pdf | md
    title: str = ""
    content: str = ""
    out_path: str


@app.post("/generate-artefact")
async def generate_artefact(req: GenerateArtefactRequest) -> dict[str, Any]:
    from . import generate

    try:
        # Offload the blocking pandoc/WeasyPrint subprocess off the event loop (L7).
        return await asyncio.to_thread(
            generate.generate_artefact, req.kind, req.title, req.content, safe_path(req.out_path)
        )
    except RuntimeError as e:
        raise HTTPException(status_code=503, detail=str(e)) from e
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e)) from e


# --- Memory recall index -----------------------------------------------------


class MemUpsertRequest(BaseModel):
    scope_key: str
    fact_id: str
    content: str
    # Provider overrides ({embed_base_url/_model/_api_key}); empty ⇒ .env defaults.
    overrides: dict | None = None


class MemSearchRequest(BaseModel):
    scope_key: str
    query: str
    limit: int = 10
    # Provider overrides ({embed_base_url/_model/_api_key}); empty ⇒ .env defaults.
    overrides: dict | None = None


class MemDeleteRequest(BaseModel):
    scope_key: str
    fact_id: str


@app.post("/memory/upsert")
async def memory_upsert(req: MemUpsertRequest) -> dict[str, Any]:
    from . import memory, rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    await memory.upsert(req.scope_key, req.fact_id, req.content)
    return {"ok": True}


@app.post("/memory/search")
async def memory_search(req: MemSearchRequest) -> dict[str, Any]:
    from . import memory, rag_ctx

    rag_ctx.set_overrides(req.overrides or {})
    return {"ids": await memory.search(req.scope_key, req.query, req.limit)}


@app.post("/memory/delete")
async def memory_delete(req: MemDeleteRequest) -> dict[str, Any]:
    from . import memory

    await memory.delete(req.scope_key, req.fact_id)
    return {"ok": True}


# --- Voice (STT / TTS) -------------------------------------------------------


class SpeechRequest(BaseModel):
    text: str
    voice: str | None = None
    # Provider overrides: tts_base_url/_model/_api_key.
    overrides: dict | None = None


@app.get("/voice/available")
async def voice_available() -> dict[str, bool]:
    from . import stt as stt_mod
    from . import tts as tts_mod

    return {"stt": stt_mod.available(), "tts": tts_mod.available()}


@app.post("/transcribe")
async def transcribe(request: Request) -> dict[str, str]:
    """Raw audio bytes (Content-Type = the audio mime) → {text}. Rust posts the
    captured audio here; we re-package as the OpenAI multipart contract. Provider
    overrides ride the `X-PAI-Overrides` header (the body is audio)."""
    import json as _json

    from . import rag_ctx
    from . import stt as stt_mod

    ov_header = request.headers.get("x-pai-overrides")
    if ov_header:
        try:
            rag_ctx.set_overrides(_json.loads(ov_header))
        except ValueError:
            pass

    if not stt_mod.available():
        raise HTTPException(status_code=503, detail="STT unavailable")
    audio = await request.body()
    if not audio:
        raise HTTPException(status_code=400, detail="empty audio body")
    mime = request.headers.get("content-type")
    language = request.query_params.get("language")
    try:
        text = await stt_mod.transcribe(audio, mime, language)
    except Exception as e:  # noqa: BLE001
        raise HTTPException(status_code=503, detail=f"STT failed: {e}") from e
    return {"text": text}


@app.post("/speech")
async def speech(req: SpeechRequest) -> Response:
    """{text, voice?} → audio bytes (Content-Type from the engine)."""
    from . import rag_ctx
    from . import tts as tts_mod

    rag_ctx.set_overrides(req.overrides or {})
    if not tts_mod.available():
        raise HTTPException(status_code=503, detail="TTS unavailable")
    if not req.text.strip():
        raise HTTPException(status_code=400, detail="empty text")
    try:
        audio, mime = await tts_mod.synthesize(req.text, req.voice)
    except Exception as e:  # noqa: BLE001
        raise HTTPException(status_code=503, detail=f"TTS failed: {e}") from e
    return Response(content=audio, media_type=mime)


# --- Optional routers ----------------------


def register_optional_routers(app: FastAPI, subpackage: str = "enterprise") -> list[str]:
    """Include any `APIRouter` named `router` exposed by a module under
    `{__package__}.{subpackage}`. The Enterprise overlay (e.g. the moderation
    `/classify-prompt` route) registers here; an absent package is a no-op, so the
    Core-ML image ships without it and those routes simply 404. Returns the names of
    the modules whose router was included."""
    pkg_name = f"{__package__}.{subpackage}"
    included: list[str] = []
    try:
        pkg = importlib.import_module(pkg_name)
    except ModuleNotFoundError:
        return included
    for info in pkgutil.iter_modules(pkg.__path__):
        mod = importlib.import_module(f"{pkg_name}.{info.name}")
        router = getattr(mod, "router", None)
        if router is not None:
            app.include_router(router)
            included.append(info.name)
    return included


register_optional_routers(app)
