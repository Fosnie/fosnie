# PDF OCR input contracts and embedding promotion

Fixed OCR profiles now expose an optional `ocr_pdf_mode`: `document` or
`page_images`. Routing settings lets the operator choose the endpoint's input
contract. The field applies only to OCR; the Core save API and ML target validator
reject it on other fixed roles. Existing profiles/defaults remain document mode.
Saving changes the profile revision, so queued plans cannot silently adopt a
different input representation. The resolved fixed target carries the chosen
mode with the existing model, endpoint, credentials and budget. Core owns this
open-source contract and Enterprise consumes it without a separate inference path.

Document mode retains whole-PDF service dispatch. Page-images mode uses native
text where available and renders only pages below the native-text threshold for
the same image OCR endpoint. Each scanned page is one accounted fixed OCR call;
its original page number is retained. Native PDF text needs no OCR assignment.
Routed mode comes from the pinned profile, overriding process/request defaults;
legacy callers may explicitly set `OCR_PDF_MODE` and otherwise retain document.
There is no endpoint/model guessing, retry or fallback to a different service.

The PDFium adapter uses144dpi PNGs, at most8 scanned pages,8million pixels per
page,32million pixels total,2MiB PNG per page and8MiB total. The remaining OCR
grant is checked before rendering and again before inference. A caller granting
only one OCR attempt cannot process two scanned pages: it fails before model
dispatch rather than increasing its allowance. Rendered bytes also pass the
existing per-call fixed input/output limits. Native extraction can precede these
checks. Large/invalid geometry and missing grants do not start raster/model calls.

Rendering is offloaded from the async loop and all adapter PDFium calls share a
process mutex. Document/page/bitmap/Pillow handles are explicitly closed. A
cancelled render may finish its bounded CPU work, but cannot start late OCR calls.
Renderer dependencies are already installed/locked through pdfplumber; pypdfium2
is now an explicit root dependency `>=5.10.1,<6` with unchanged5.10.1 lock data.
Its wheel licence notices remain required with normal binary distribution.
The [PDFium API documentation](https://pypdfium2-team.github.io/pypdfium2/python_api.html)
describes render scale, handle closing and the threading constraint; the
[project licence notes](https://github.com/pypdfium2-team/pypdfium2#licensing)
identify the wrapper/PDFium licences. No PyMuPDF or Marker was introduced.

V27 actual Core upload/scheduler/ML verifies DOCX, native2pagePDF, image-only
2pagePDF and a mixed native+scanned PDF. The installed GLM-OCR receives PNG input:
2 calls for the wholly scanned document,1 for the mixed document,0 for native
PDF/DOCX. Page numbers1/2 and the small numeric transcription probes survive.
Contexts and parent-child links are retained. The nine-page case fails before
model calls and leaves earlier indexes untouched. An injected desired503 also
preserves old points/parents; a shorter replacement clears stale chunks.

Two actual encoders are used: BGE-M3 and `qwen3-embedding:0.6b`, both1024dimensions.
Before promotion, two physical collections contain the same7 logical points and
parents, with different real dense-vector hashes. This confirms distinct encoding,
not a retrieval-quality improvement. The new model was pulled once into a task-owned
workspace model store on a separate temporary Ollama server; the main model store
was not changed. Its [official tag](https://ollama.com/library/qwen3-embedding:0.6b)
is the model download source, not a benchmark certificate.

The private consumer calls the normal ML alias-swap operation, promotes Core index
provenance and saves a new active embedding profile. These control routes exist
only in the isolated example. After promotion, a new upload resolves Qwen as active
with no desired assignment and appears only in the new physical collection:
Qwen8points, old BGE7points. The live protocol does not exercise a full production
reindex of a large existing corpus, different vector dimensions, an interrupted
promotion or post-promotion RAG quality. Cross-collection persistence/alias-plus-
Postgres recovery still needs explicit fault coverage; no distributed atomicity
or storage rollback is claimed.

All9 operational scenarios pass. Live32 upstream calls: Qwen text17, BGE6, Qwen
embedding6, GLM-OCR3; all200. Proxy endpoint names retain embed_desired for Qwen's
URL after promotion, but the Core job receipt correctly records its role as active
Embed. One extra desired503 reservation is refused before upstream, without a
retry/refund. Cloud0. Core68routing tests pass/1ignored, affected ML67pass, both
Core/Enterprise alltargets and frontend types pass, lock checks offline. Initial
test-consumer/API assumption errors and a synthetic sparse-vector digest error
are retained and fixed before live; no failed claim was replayed.

Published text is reused and OCR probes are deliberately small. This is routing/
accounting/format/migration evidence, not independent semantic quality or P12
admission. All private schemas/services/indexes and two model processes are
removed after the run. The task-owned639MB embedding cache is retained in the
workspace for reuse; original models, production settings and accounts stay
untouched. See workspace report.v27.md, JSON, native ledger and cleanup receipts.


The later [V28 index recovery protocol](model-routing-index-recovery.md) adds
model-free document compensation and durable embedding publication intent.
Earlier storage-failure limitations above describe their historical runs.
