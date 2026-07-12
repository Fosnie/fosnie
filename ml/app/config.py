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

"""Boot config for the ML service. Env-driven so the same build serves dev
(Ollama) and prod (vLLM) — the platform is not engine-locked. All LLM access
is OpenAI-shape; swapping engines is a base-URL/model change."""

from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_file=".env", extra="ignore")

    # OpenAI-compatible endpoint. Dev: Ollama. Prod: vLLM.
    llm_base_url: str = "http://localhost:11434/v1"
    # Dev LLM. Lighter 4B = faster decompose/grade/generation in Ollama.
    llm_model: str = "huihui_ai/qwen3.5-abliterated:4b-Claude"
    llm_api_key: str = "ollama"  # Ollama ignores it; vLLM may require a real key.

    # Fallback context window when the engine does not advertise it via
    # /v1/models (Ollama omits it; vLLM reports max_model_len).
    llm_max_model_len: int = 32768

    # Default generation cap applied when an Agent doesn't set max_tokens. Bounds
    # runaway/looping models (so a turn finishes and its token usage is recorded);
    # agents can still override. Raise per deployment for longer answers.
    # NOTE: with a thinking model this budget covers BOTH the reasoning tokens and
    # the answer. On OpenAI reasoning models (gpt-5.x / o-series) it is spent as
    # `max_completion_tokens`, which the model can exhaust entirely on reasoning for
    # a hard, multi-part prompt — returning an EMPTY answer (finish_reason="length").
    # 2048/8192 were both too tight for that case, so the floor is 32768; the backend
    # additionally retries once with a bumped cap on an empty `length` finish.
    llm_default_max_tokens: int = 32768

    # Ask the engine to emit token usage in the stream (vLLM supports
    # stream_options.include_usage; off by default to stay safe with Ollama).
    llm_stream_usage: bool = False

    # on an OpenAI reasoning model (gpt-5.x/o-series) with reasoning
    # enabled + trace on, stream SUMMARISED thoughts before the first answer token via
    # the Responses API (`reasoning.summary="auto"`) — chat-completions does not surface
    # them. Any endpoint/model rejection falls back to chat-completions, so this is safe
    # to leave on; set false to force the chat-completions path. No effect off OpenAI
    # (vLLM-qwen3 / Anthropic / Gemini already stream reasoning natively).
    openai_reasoning_summaries: bool = True

    # Constrain the structured loop calls (decompose / grade / plan / outline /
    # triage / census / notes) with vLLM guided decoding (xgrammar). vLLM-only:
    # Ollama / llama.cpp-server ignore the `guided_*` shape, so this is OFF by
    # default and set true ONLY in the vLLM profile (.env.linux.example). When
    # off, the loops keep their prompt-only path verbatim (the defensive
    # JSON-extraction fallbacks stay in place either way). See app/guided.py.
    llm_guided_decoding: bool = False

    # Omit sampling params (temperature/top_p/top_k/frequency_penalty/
    # presence_penalty) from LLM requests. Some endpoints reject them: Anthropic's
    # OpenAI-compatible shim returns 400 `temperature is deprecated for this model`
    # for the Claude 4.6+ family (temperature/top_p/top_k removed). Set true when
    # the configured provider is such an endpoint. Default false (vLLM/Ollama/
    # llama.cpp all accept them).
    llm_omit_sampling: bool = False

    # Extended thinking for the NATIVE Anthropic path (anthropic_adapter). Format:
    # "off" | "adaptive[:effort]" | "budget:<N>". adaptive = Opus 4.6+/Sonnet 4.6
    # (effort low|medium|high|xhigh|max via output_config); budget = older models
    # (N>=1024, must be < max_tokens). Model-dependent and NOT auto-detected — a wrong
    # mode 400s and the body is logged. Default off (Phase-1 behaviour). Ignored on the
    # OpenAI path.
    llm_thinking: str = "off"

    # Embeddings (OpenAI-shape). Dev: Ollama bge-m3. Prod: Qwen3-Embedding service.
    embed_base_url: str = "http://localhost:11434/v1"
    embed_model: str = "hf.co/ggml-org/bge-m3-Q8_0-GGUF:Q8_0"
    embed_api_key: str = "ollama"

    # Reranker via a proper /v1/rerank endpoint. Dev: llama.cpp server serving
    # the Qwen3-Reranker GGUF (Ollama can't serve rerankers). Prod: Qwen3-Reranker
    # service (Infinity). base_url has no /v1 suffix — code appends /v1/rerank.
    rerank_enabled: bool = True
    rerank_base_url: str = "http://localhost:8091"
    rerank_model: str = "Qwen3-Reranker-0.6B"
    rerank_api_key: str = "none"
    # Reranker-resilience. A multi-part prompt fans ~6 sub-Q ×
    # 3 variants ≈ 18 rerank calls out at once; a rate-limited/free reranker then 429s
    # and used to degrade SILENTLY to a 0.0 score (which wrecks top-k order + the
    # grade-gate). A dedicated semaphore throttles the burst independently of
    # retrieve_concurrency, and a short capped exponential backoff rides out a 429/5xx/
    # timeout before the caller falls back to the hybrid-fusion score.
    rerank_concurrency: int = 3      # max concurrent /v1/rerank calls (independent of retrieve_concurrency)
    rerank_max_retries: int = 3      # attempts on 429/5xx/timeout before giving up to the cooldown
    rerank_backoff_base: float = 0.5 # seconds; exponential (base·2^n), capped so the tail stays ≤120s

    qdrant_url: str = "http://localhost:6333"
    # Qdrant client request timeout (s). Generous so a delete-by-filter / upsert
    # under heavy concurrent ingest load does not ReadTimeout and fail the whole
    # ingest (which then retry-loops Error->Indexing). Tunable via QDRANT_TIMEOUT.
    qdrant_timeout: float = 60.0

    # Agentic retrieval knobs (per-Agent override is Pass-2).
    top_k: int = 8           # chunks per sub-query after rerank
    over_retrieval: int = 4  # multiplier searched before rerank
    # Agentic loop cap. Spec cap is 5–6; default kept low for the slow dev LLM
    # (each round runs grade/reformulate calls). Prod tunes up via MAX_ROUNDS.
    max_rounds: int = 2
    # ~3 queries per sub-question (multi-query): the original +
    # LLM reformulations, run concurrently. Caps the per-round fan-out width.
    query_variants: int = 3
    # Max concurrent search pipelines (embed→hybrid→rerank) across all
    # sub-questions/variants — bounds load on the embed/Qdrant/reranker backends
    # while retrievals run in parallel.
    retrieve_concurrency: int = 4
    # Grade-gate: skip the per-sub-question LLM grade CALL when the TOP reranker score
    # clears this. The /v1/rerank score scale is server-dependent (Infinity ≈ 0–1 sigmoid;
    # Jina ~0.5-centred; llama.cpp raw logit), so there is NO safe universal default —
    # ships OFF (0.0). CALIBRATE per deployment from the eval `best_rerank` distribution
    # (percentile that fires only on confident hits), set via the super-admin
    # `rag.grade_skip_threshold` knob. NOTE: the gate only skips the
    # grade *call* — it never marks the round resolved, so a reformulate round still runs
    # (an over-low threshold no longer silently kills recall).
    grade_skip_threshold: float = 0.0

    # Sub-question isolation + synthesis. Each sub-question is answered
    # in an ISOLATED mini-context (only its own reranked passages), then the final answer
    # synthesises from the labelled sub-answers over one consolidated [D#] document pool —
    # this is what stops one scenario's chunks contaminating another. Mini-answers run on
    # the fast utility model at minimal reasoning, in parallel, fail-soft. Disable to fall
    # back to the plain merged-context behaviour (also the local/no-LLM degradation path).
    sub_answer_enabled: bool = True
    sub_answer_max_tokens: int = 600 # room to cite EVERY relevant extract, not just 2-4
    sub_answer_chunks: int = 8         # passages shown to one mini-answer (defaults to top_k)
    sub_answer_timeout: float = 40.0   # per mini-answer; on timeout the sub-Q is marked failed
    sub_answer_check: bool = False     # optional cheap yes/no "does the answer address it?"
    pool_floor_chunks: int = 3         # direct-retrieval chunks added as a bad-decomposition floor
    # a fast mini-answer only cites 2-4 chunks, so relevant-but-uncited
    # chunks would otherwise vanish from synthesis. Also keep the top-N reranked
    # UNcited chunks per sub-question in the [D#] pool. 0 = cited-only behaviour.
    pool_uncited_per_subq: int = 3
    # The [D#] pool budget scales with the number of sub-questions so a
    # 5-6 part prompt is not starved by the flat max_context_chunks cap:
    #   pool_budget = min(pool_hard_cap, max(max_context_chunks, N_subq * pool_per_subq_budget))
    # per-sub-Q budget ~ 3 cited + 3 uncited; hard cap bounds worst-case final-prompt tokens.
    pool_per_subq_budget: int = 6
    pool_hard_cap: int = 48
    # cross-referenced / neighbour operative-text chunks (required
    # anchors, refs_out, ±N neighbours) are precisely targeted, so they get a RESERVED slice
    # of the pool ON TOP of pool_budget — a generic uncited chunk can never evict a fetched
    # cross-reference (the "followed but not pooled" bug). Total pool ≤ pool_budget + this.
    # The topic→section (table-of-contents) channel also feeds this reserve, and a many-sub-
    # question legal query (≈12) needs a few round-robin tiers of depth for a broad topic to pool
    # its full section span. Additive (separate counter, `hard_ceiling` grows with it) so it never
    # lowers cited/uncited-tier recall.
    pool_crossref_reserve: int = 40
    # when a mini-answer says NOT IN CONTEXT, do ONE targeted retry —
    # pull section refs / legal terms from the sub-question, retrieve BM25-heavy, re-answer.
    # Off = today's single-shot honest-fail behaviour.
    targeted_fallback_enabled: bool = True

    # Multi-part prompt handling (super-admin knobs). How many
    # atomic sub-questions a prompt decomposes into, how many distinct parent
    # sections feed the LLM, and total merged chunks — raised so a complex,
    # multi-question prompt doesn't lose whole questions (decomposition is the point).
    # With per-sub-question mini-answers each sub-Q costs one utility
    # LLM call, so the default is trimmed from 10 to protect latency; raise per deployment.
    max_subqueries: int = 6
    # a numbered N-part prompt must never lose a whole question to
    # the flat cap. The coverage-guardrail raises the effective cap to
    # max(max_subqueries, n_numbered_parts + 2), bounded by this ceiling (DoS/latency
    # guard). Trimming (if still needed) is proportional across parts, never a tail slice.
    max_subqueries_ceiling: int = 12
    max_parents: int = 16 # parent sections handed to the synthesis LLM
    max_context_chunks: int = 24

    # Deterministic retrieval expansion. Pure Qdrant look-ups — no
    # extra LLM/embedding on the hot path — so a sub-question's REQUIRED sections
    # (anchors named in the prompt) and the sections its top chunks cross-reference, plus
    # their immediate neighbours, are always fetched. This is the Q4/Q5 fix.
    anchor_lookup_max: int = 8 # required-anchor payload look-ups per turn, fail-soft
    neighbor_span: int = 1 # ± section_num neighbours to pull per found section
    crossref_max_sections: int = 8 # cross-referenced/neighbour sections fetched per sub-Q
    # TOC channel: a topical (numberless) sub-question is matched to a
    # statute chapter by title, then this many contiguous section numbers are swept from the
    # chapter start (statute chapters are adjacent, so ~24 reaches the sibling chapter — e.g.
    # Allotment→Pre-emption). 0 = channel off.
    toc_max_sections: int = 24
    # last guardrail: before a per-part synthesis can call a section
    # "not reproduced", if the part NAMES that section's number but its slice lacks it, force a
    # fetch_by_sections and inject it (or reuse an already-pooled block for attribution). This
    # many per part, fail-soft. 0 = guardrail off.
    late_anchor_cap: int = 4
    # after mini-answers + slice assembly and BEFORE synthesis, a bounded
    # gap-check LLM call (per part / per turn) names specific provisions still missing; a
    # deterministic fill (fetch_by_sections + BM25 + TOC) tops up the slice from a non-evictable
    # append budget. Reuses the whole retrieval toolkit; late_anchor stays the final guardrail.
    gap_round_enabled: bool = True    # gate the whole phase (mirror rerank_enabled bool)
    gap_rounds: int = 1               # gap-check→fill iterations (re-check only if fill added)
    gap_reserve: int = 12             # max [D#] blocks the gap phase may append per turn

    # OCR (.md): GLM-OCR over an OpenAI-compatible
    # vision endpoint. The OCR *service* handles each file — including
    # rasterising scanned PDFs — so the platform never rasterises (PyMuPDF/Marker
    # struck on licence; Docling MIT is the abstracted fallback). Office formats
    # (DOCX/XLSX/PPTX) use native parsers and never touch OCR. When a scanned PDF
    # or image needs OCR and it is disabled/unreachable, ingestion fails loudly
    # (status=error) rather than indexing empty text.
    ocr_enabled: bool = True
    ocr_base_url: str = ""  # blank → fall back to llm_base_url
    ocr_model: str = "hf.co/ggml-org/GLM-OCR-GGUF:Q8_0"
    ocr_api_key: str = "ollama"
    ocr_timeout: float = 180.0  # OCR is slower than chat; give it room

    # Whole-document modes (.md). Deterministic token
    # routing: a document is STUFFED whole when it fits a safe fraction of the
    # context window; otherwise an EXHAUSTIVE map-reduce reads every section.
    stuff_fraction: float = 0.45    # of max_model_len that a stuffed doc may use
    map_window_chars: int = 6000    # ~1.5k tokens per map section (semantic-ish)
    map_concurrency: int = 4        # concurrent section maps (bounded vs the LLM)
    # Safety cap for read_document's map-reduce: an agent pointed at a huge file
    # would otherwise map-reduce the WHOLE thing (hundreds of LLM calls, minutes,
    # engine saturation). Beyond this many chars the text is capped — for a large
    # corpus the agent should rely on RAG retrieval, not read the file whole.
    read_document_max_chars: int = 120000

    # Chunker (Petro's proven defaults; configurable). Unit = CHARACTERS
    # (unit is chars for now; tokens are a future re-tune).
    chunk_size: int = 1500
    chunk_overlap: int = 400
    # PDF extractor: pdfplumber (table-aware — recovers row/column structure pypdf
    # flattens) when on; fast pypdf text when off. Live knob: ingest.pdfplumber.
    ingest_pdfplumber: bool = True

    # Layered chunker upgrades, per-deployment, default OFF
    # so the L0+L1 base behaviour is unchanged unless explicitly enabled.
    # L2 parent–child: embed small children, return the enclosing parent section.
    parent_child: bool = False
    child_chunk_size: int = 700      # chars; one clause/paragraph-ish
    child_chunk_overlap: int = 150
    parent_chunk_size: int = 2400    # chars; the enclosing section
    # L3 contextual retrieval: prepend an LLM situating blurb before embedding.
    contextual_retrieval: bool = False
    contextual_doc_budget: int = 6000   # chars of surrounding doc fed to the blurb
    contextual_max_chunks: int = 200    # cost guard; beyond this, skip the blurb
    contextual_concurrency: int = 4     # parallel blurb calls per ingest (bounded fan-out)

    # Voice — STT + TTS as external OpenAI-audio HTTP engines,
    # behind a swappable abstraction (only the model/URL differs per deployment).
    # Separate servers/ports: STT via /v1/audio/transcriptions, TTS via
    # /v1/audio/speech. Off by default — enable per deployment.
    # Dev (Mac/llama.cpp): Qwen3-ASR GGUF (STT) + OmniVoice GGUF (TTS).
    stt_enabled: bool = False
    stt_base_url: str = "http://localhost:8092"
    stt_model: str = "qwen3-asr"
    stt_api_key: str = "none"
    # STT wire shape. "openai" = POST /v1/audio/transcriptions (multipart);
    # "chat" = POST /v1/chat/completions with an input_audio part (raw llama.cpp
    # multimodal ASR, e.g. Qwen3-ASR — no native /v1/audio/* endpoint).
    stt_format: str = "openai"
    # Optional spoken-language hint (ISO code, e.g. "en"). Empty = let the engine
    # auto-detect. A small multilingual ASR (Qwen3-ASR) mis-detects short/accented
    # English as another language, so the English-first profile pins this to "en".
    # Applied in BOTH wire shapes (chat-prompt hint + the multipart `language` field).
    stt_language: str = ""
    tts_enabled: bool = False
    tts_base_url: str = "http://localhost:8093"
    tts_model: str = "omnivoice"
    tts_api_key: str = "none"
    tts_voice: str = "default"
    tts_format: str = "wav"  # response_format for /v1/audio/speech

    # Groundedness verifier — a small
    # cross-encoder behind a swappable interface, like the reranker/OCR. Mode A
    # (live) is LettuceDetect, which self-highlights unsupported spans over
    # (context, question, answer). External HTTP engine on GPU2 or CPU; base_url
    # has NO /v1 suffix — code appends /v1/verify. Off by default; enable per
    # deployment. Dev: the reference sidecar in backend/deploy/verify (port 8095).
    verify_enabled: bool = False
    verify_base_url: str = "http://localhost:8095"
    verify_model: str = "KRLabsOrg/lettucedect-large-modernbert-en-v1"
    verify_api_key: str = "none"
    verify_timeout: float = 60.0  # the verifier may be slower than the reranker
    # Mode B ("Verify draft"): claim-level FactCG verifier + cost bounds.
    verify_factcg_model: str = "yaxili96/FactCG-DeBERTa-v3-Large"
    verify_draft_max_claims: int = 200    # cost cap on a single draft job
    verify_draft_evidence_k: int = 4      # top-k retrieved chunks bound to each claim
    verify_claims_batch: int = 16         # claims per /v1/verify-claims call

    # Web search — dormant connector; the Rust egress
    # gate decides whether /web_search is ever called. SearXNG is the only v1
    # provider (resolved decision 1); the interface stays swappable.
    web_search_provider: str = "searxng"
    searxng_base_url: str = "http://localhost:8888"
    web_top_results: int = 10      # SERP results considered per query
    web_fetch_top_n: int = 4       # pages actually fetched
    # Agentic-loop rounds per sub-question (standard depth); deep gets its own.
    # Clamped to max_rounds_ceiling like the RAG loop.
    web_max_rounds: int = 2
    web_deep_rounds: int = 4
    # Wall-clock budgets per depth class — on expiry the loop jumps straight to
    # best-effort assembly ("beast mode") rather than failing. Standard sits
    # under the 120 s Web tool timeout with headroom.
    web_wall_clock_quick: float = 30.0
    web_wall_clock_standard: float = 90.0
    web_wall_clock_deep: float = 900.0
    # Shingle-Jaccard threshold for syndication dedup (lead text).
    web_dedup_threshold: float = 0.6
    # In-memory TTL caches (seconds; 0 disables). SERP results cache only when
    # non-empty (never cache an outage); pages cache post-extraction.
    web_serp_cache_ttl: float = 600.0
    web_page_cache_ttl: float = 3600.0
    # Runtime-overridable policy knobs (per-request overrides from the Rust
    # backend take precedence — see rag_ctx.cfg()).
    web_allowlist_only: bool = False
    # "user_triggered" (default; single user-requested fetches proceed — the
    # major-assistant posture, resolved decision 4) or "respect" (robots.txt
    # honoured per host, fail-open on robots fetch errors).
    web_robots_policy: str = "user_triggered"
    web_robots_cache_ttl: float = 3600.0
    web_fetch_timeout: float = 15.0
    web_connect_timeout: float = 5.0
    web_fetch_max_bytes: int = 8 * 1024 * 1024
    web_max_redirects: int = 5     # each hop re-validated by the SSRF guard
    web_fetch_concurrency: int = 3
    # Honest, stable fetcher UA (resolved decision 4) — never a spoofed browser.
    web_user_agent: str = "PAIPlatform/1.0 (+https://private-ai.example)"
    # Playwright render escalation + rendered fallback search. Off by default:
    # the service must boot and pass tests without Chromium installed.
    web_render_enabled: bool = False
    web_render_timeout: float = 25.0
    # Politeness pacing (the no-IP-ban guarantee): token-bucket rates per search
    # engine and per fetched host, requests/second, with a small burst.
    web_engine_rps: float = 1.0
    web_host_rps: float = 0.5
    web_pacing_burst: float = 2.0
    # Comma-separated domain suffix lists; empty = off. Blocklist wins. (The
    # IP-level SSRF guard is always on regardless.)
    web_domain_allowlist: str = ""
    web_domain_blocklist: str = ""

    # Deep Research. Budgets derive from the
    # runtime max_model_len (research/budgets.py); these bound the run itself.
    # Clock ordering: research_max_minutes < the Rust stream timeout (45 min)
    # ≈ the agent-run kill-token TTL minted at start.
    research_max_minutes: float = 20.0
    research_notes_concurrency: int = 4
    # Corpus census cap (Phase 2). At or below this many documents the corpus is
    # read in full (a per-doc structured note each); above it, the run falls back
    # to agentic-retrieval sampling with an honest "documents not reviewed"
    # appendix. Dev boxes with a slow local LLM should tune this LOW (e.g. 60) —
    # 1–3 LLM calls per document, so 500 docs is hours, not minutes.
    research_census_cap: int = 500

    # Read timeout for the shared inference client. Generous: a reasoning model on the
    # NON-streaming /chat-step (tool loop) can think for minutes with no bytes flowing,
    # so 300s used to time out → a raw 500. Streaming /generate resets
    # this per chunk, so raising the ceiling is harmless there.
    request_timeout: float = 600.0
    # Tighter read timeouts for embeddings/rerank than the generous streaming
    # `request_timeout` — but generous enough for the
    # slow paths that share them: tabular embeds a whole document's chunks in
    # one request and ingest sends 64-chunk batches, both >30 s on a CPU
    # embedder; a rerank timeout also trips the reranker's failure cooldown
    # 120 s ≈ 2.5× tighter than before without breaking those.
    embed_timeout: float = 120.0
    rerank_timeout: float = 120.0
    port: int = 8090

    # Shared secret the Rust backend sends as `X-PAI-ML-Key`. When set, every
    # request (bar /health) must carry the exact value. Empty (default) = open,
    # for localhost dev; set SHARED_SECRET in production.
    shared_secret: str = ""
    # Filesystem root that all request-supplied `path`/`out_path` values must
    # stay within (defence-in-depth path confinement). Empty = skip the check
    # (dev, where paths are relative to each service's CWD).
    storage_root: str = ""
    # Hard ceiling on the agentic retrieval loop regardless of max_rounds, and a
    # request-body byte cap, to bound worst-case cost.
    max_rounds_ceiling: int = 8
    max_request_bytes: int = 32 * 1024 * 1024

    # DOCX→PDF rendition via LibreOffice headless. `soffice_bin` is looked up on
    # PATH first; render.py also probes common Windows install paths.
    soffice_bin: str = "soffice"
    soffice_timeout: float = 120.0  # cold start of headless soffice is slow

    # Beautiful documents.
    # Primary DOCX route: pandoc Markdown→DOCX against a styled reference document
    # (the swappable-engine empirical choice). `pandoc_bin` resolved on PATH first;
    # docx.py probes common install paths. When pandoc is absent the structural
    # python-docx builder is used instead — both deployment profiles stay working.
    pandoc_bin: str = "pandoc"
    pandoc_timeout: float = 60.0
    # ML file logging. uvicorn ships console-only, so a diagnostic
    # run is lost when the process restarts. A RotatingFileHandler on
    # the root logger persists INFO+ (incl. the `rag turn …` summary) to `<log_dir>/pai-ml.log`.
    # log_dir resolves relative to the process CWD (the container WORKDIR /opt/pai/ml → /opt/pai/ml/data).
    # Empty log_dir disables the file handler (console only).
    log_dir: str = "./data"
    log_max_bytes: int = 10_000_000  # ~10 MB before rotation
    log_backups: int = 3             # rotated files kept
    # The neutral brand reference.docx (all heading/body/table/TOC styles). Generated
    # on first use if absent; a deployment points this at its own branded file.
    docx_reference: str = "./data/branding/reference.docx"
    # Primary PDF route: Markdown→HTML→WeasyPrint against a print stylesheet (CSS
    # Paged Media). `pdf_css` empty = the bundled neutral default
    # (app/assets/pdf/report.css); a deployment points it at its own brand CSS.
    # When WeasyPrint's native libraries are absent (e.g. the Windows dev box) the
    # LibreOffice DOCX→PDF path is used instead.
    pdf_css: str = ""

    # HTML artefacts: self-contained, offline-
    # portable pages (dashboards; the Deep Research "Create page" button). The model
    # writes content with `<!-- pai:echarts -->` / `<!-- pai:theme -->` markers; the
    # html engine inlines the vendored ECharts build + theme there and injects a
    # strict CSP <meta> (zero-egress inside the artefact). `html_theme` empty = the
    # bundled neutral default (app/assets/html/theme.css); a deployment overrides it
    # to brand. (The one-shot HTML generation for the "Create page" button is driven
    # by the Rust backend via /chat-step, which sets its own max_tokens.)
    html_theme: str = ""

    # Validate generated DOCX `word/document.xml` against the bundled ISO/IEC 29500-4
    # (Transitional) OOXML XSD schemas. On by
    # default when the schemas are present; a schema-validation failure is logged as a
    # warning rather than rejecting the document (the reopen + LibreOffice-opens checks
    # remain the hard gate) — set strict separately if a deployment wants hard XSD.
    docx_xsd_validate: bool = True

    # Tabular review: simultaneous per-cell LLM calls (bounded so vLLM's
    # continuous batching is not overwhelmed and the active chat is not evicted).
    tabular_concurrency: int = 4
    # Top-k chunks for per-document retrieval (per_document_rag / over-budget).
    tabular_topk: int = 6


settings = Settings()
