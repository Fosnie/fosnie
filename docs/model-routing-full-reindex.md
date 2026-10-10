# Accounted full-corpus rebuild and retired-index cleanup

POST `/api/admin/embedding-index/reindex` now creates a routed durable job with
the requesting principal and its effective deployment/personal/edition limits.
The runtime is fixed-only: it pins active Embed as a read-only source reference
and EmbedDesired as the embedding executor. No LLM, classifier, fallback or
quality-ranked embedding substitution occurs. A private fixed-rebuild constructor
and durable format retain one original ledger, absolute deadline and references.
Keys are resolved only at dispatch and never enter the saved checkpoint.

The caller needs `providers.manage`, management access to every live library and
no denied source documents under the installed RbacPolicy. A privileged provider
manager without full corpus access is refused instead of receiving a fabricated
service/admin principal. Queue/worker/monitor/dispatch recheck actor, permissions,
corpus IDs and active/desired index revisions. Limits cap the scope to1024 live
libraries and65536 document IDs. Archived/orphan points are outside that scope and
cause strict preflight refusal if encountered. This is an explicit conservative
full-corpus operation, not per-user filtering that would silently discard data.

ML snapshots bounded sorted point IDs/payloads before embedding, validates every
point against the Core-authorised KB/document scope and checks needed batches
against its fixed grant. Snapshot bounds are32768points/64MiB. Batches are64texts;
each HTTP attempt consumes its original `embed_desired` allowance. The job cap is
512calls/30minutes further narrowed by policy and deployment limits. A default
128call policy therefore supports at most8192 chunks. Token/cost totals for fixed
services remain unknown where unproven; bytes/attempts are separate accounting.

An explicit new attempt can reset only an unaliased desired collection before
building it. It cannot reset the active source or a collection serving an alias.
It reconstructs contextual embedded text from original payloads. A second corpus
digest/alias/count check rejects changed source data before a terminal publication
receipt. Full scope and source revision checks also run in Core. This detects
corpus drift; it is not an online incremental rebuild guarantee under continuous
ingestion. Partial desired output remains unpublished on failure. New attempts
are explicit new jobs/ledgers; started jobs never replay their model phase.

The fixed report is reconciled before a fenced durable built checkpoint. Existing
model-free promotion then publishes alias, SQL provenance, dimensions and adopted
active profile and settles the linked routed job. Lost publication replies can
resume that storage phase without inference. Public task payloads contain only
routed_job_id; duplicate/wrong delivery cannot adopt another ledger. Legacy queued
maintenance is excluded when launching the new route. Older legacy rebuild
attempt records retain their non-replay rule but are not reclassified as routed.

Confirmed promotions record the retired physical source, including the literal
backup name when converting the first literal collection. The provider admin UI
offers **Clean retired indexes** through POST `/api/admin/embedding-index/cleanup`.
Cleanup preserves the newest eligible retired index, requires at least24hours,
handles at most16 candidates per request and leaves recent sources in place.
Core rechecks current/desired targets, pending publication and active rebuild
references under the index-row lock. ML checks the current alias again under the
shared storage fence and refuses protected collections or any other alias target.
Missing already-deleted candidates settle idempotently after a lost reply.

Cleanup is explicit, not a background purge, and records a deletion audit. The
default policy may conservatively retain an extra older eligible index when the
newest overall retirement is less than24hours old. There is no arbitrary-name
deletion API exposed to an ordinary user. Long-lived external Qdrant readers,
independent storage roots and cross-store power-loss behaviour remain outside
the V28/V29 protocol. Shared-root fencing, recovery journals and retained sources
remain required. Retention defaults are deliberately fixed in this first version.

V29 actual Core endpoint -> durable scheduler -> actual ML -> real Qdrant rebuilds
130 synthetic chunks with exactly3 synthetic embedding replies (64/64/2). The
single Core ledger records3 calls/unknown token+cost entries, with an empty LLM
assignment map and0classifier calls. Queued duplicate returns409, publication
succeeds and retains the old source. On owned aged fixtures cleanup deletes only
the older eligible collection, preserves active/current/newest and recent sources.
This is operational/accounting evidence: no real provider/model call or new cloud
allocation was used. V27 separately exercised actual BGE/Qwen encoders.

Affected ML76tests, Core68routing tests/1ignored, Core/Enterprise targets and
frontend types pass; migration136 is unified. The final canonical desired target
and legacy queue guards are compiled and exercised by the complete protocol.
Large-corpus throughput, live partial-build retry/in-flight ACL revocation and
multi-replica load still need broader evidence. Independent model quality/P12
remains separate. Read workspace report.v29.md, JSON, ledger receipt and cleanup.

V30 adds [owned cancellation and explicit fresh-attempt evidence](model-routing-reindex-cancellation.md), including actual Qwen3-Embedding batches.

V31 adds [shared-root builder locks and publication drift checks](model-routing-reindex-fences.md), with a two-process ML protocol.
