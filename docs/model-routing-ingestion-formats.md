# Ingestion formats, contextual fan-out and dual-write replacement

The shared ML ingestion pipeline now completes metadata, contextual summaries,
active embeddings and desired embeddings before replacing existing corpus data.
Any propagated failure in those model calls leaves both previous chunk sets and
shared parents untouched. This ordering applies to routed and legacy ingestion;
legacy date/context helpers retain their intentional best-effort fallbacks. The desired
collection is explicitly cleared for the document before dual-write upsert, so
shorter replacements cannot retain stale tail chunks. Both deletions precede the
new parent upsert; deleting the desired set afterwards would remove new parents.
The active alias is checked again after inference and before replacement.

These are inference-failure guarantees, not a distributed Qdrant transaction:
collection creation/delete/upsert can still fail partway through persistence.
Postgres readiness/outbox/job success remains fenced, and unknown model spend is
retained. A failed job is not automatically replayed. Core and installed editions
consume this common pipeline without an Enterprise dependency in Core.

For strict routed ingestion, native extraction determines the child count first.
Before metadata/context/embedding calls, a preflight checks remaining grants for
one date call, each requested context summary and active/desired embedding batches.
OCR may already have been necessary to obtain text and counts against its fixed
grant. Exceeding the summary allowance or configured contextual chunk cap stops
the operation; it does not silently drop contextual retrieval. Legacy ingestion
retains its configured best-effort context-cap skip. Per-call reservations remain
atomic and cannot extend the Core-issued grant. Failed context siblings are
cancelled and joined before the owner returns, preventing later model spending.

Strict dense embedding responses must contain exactly the requested integer input
indexes, one correctly sized finite numeric vector per input, and no boolean
coordinates/indexes. Duplicate indexes, missing vectors, NaN, wrong dimensions
and malformed rows fail before corpus replacement. Known office file extensions
remain authoritative when a connector mislabels a DOCX as text/plain; tracked-
change flattening runs instead of indexing ZIP bytes as text.

The private routing_ingestion_formats_pilot uses the actual Core upload handlers,
durable worker, ML parsers, helpers and index. Its server-only replacement endpoint
accepts bounded fixture bytes under the same actor/KB checks; it is not deployed.
The flow fixture optionally accepts a loopback OCR URL; existing consumers retain
their previous default. It prepares distinct active/desired collections and
separately pinned grants using the same installed BGE-M3 model and dimension1024.
This tests collection dual-writing, not migration between different encoders.

V26 loads a DOCX with intentionally incorrect text MIME, a two-page native PDF,
and a small PNG through installed local Qwen text helpers, BGE-M3 embeddings and
GLM-OCR. Context summaries are embedded separately while chunk_text remains
original. Both physical collections contain4 points from3 documents with4 valid
parents, no seeded vectors; PDF page numbers1/2 survive. A shorter version3
replaces the DOCX in both indexes. An injected desired503, contextual overflow,
bare-Ollama PDF OCR refusal and corrupt DOCX all preserve previous point IDs,
payloads and parents. Overflow/corrupt DOCX make0 native requests; all8 job
snapshots are removed. The PNG check confirms only its small numeric probe.

Live upstream calls22: Qwen11, active embed5, desired embed4, OCR2.21 return200;
the image endpoint rejects a PDF without a text layer with400. One additional
desired embedding reservation is refused by the test proxy before upstream.
There is no retry/refund or invented billing evidence; cloud0. Successful scanned
PDF OCR still requires a PDF-capable OCR service/adapter and is not established
by this refusal test. No model weights were downloaded and production settings,
real accounts and the main Ollama server were unchanged.

Permanent affected ML regressions58pass, including preservation on each inference
failure, both-index/parent ordering, zero-egress fan-out refusal, sibling joining,
malformed vectors and DOCX MIME. The private Core consumer builds, full synthetic
and local live protocols pass. Unchanged broad Rust/frontend suites were not
repeated. Published V19 text is reused; this is operational evidence, not a new
semantic model-quality cohort or P12 admission. Read workspace report.v26.md,
report.v26-summary.json and primary native ledger/receipts. Owned schemas/services,
GPU process and both temporary indexes are removed after the run.


The later [V27 PDF OCR contract and distinct-encoder promotion](model-routing-pdf-migration.md)
adds successful bounded scanned/mixed PDF ingestion. The V26 refusal above remains
a historical result of whole-PDF input to the bare image endpoint.


The later [V28 index recovery protocol](model-routing-index-recovery.md) adds
model-free document compensation and durable embedding publication intent.
Earlier storage-failure limitations above describe their historical runs.
