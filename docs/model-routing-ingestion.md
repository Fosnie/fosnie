# Routed uploaded-document ingestion

The default-off `routing.ingestion_enabled` deployment gate adopts human uploads
from the Library and Project document APIs. Providers managers can change it
through `/api/routing/ingestion` or the routing settings rollout controls.
Unauthenticated requests and stale/deactivated actor roles cannot enable it.
Connector imports have a separate default-off gate and source-ACL extension
contract; see [connector ingestion](model-routing-connector-ingestion.md).

An opted-in upload uses the uploader's effective Auto policy. Planning receives
a bounded ingestion description, never document content or a file path. Metadata
and optional contextual blurbs use the planned `utility_summary` assignment.
Embedding, desired embedding during a rebuild, and OCR retain separate pinned
fixed-service references. Fixed availability is checked before planning. Model
effort and policy/profile/index revisions remain pinned; helpers cannot use ML
process defaults or silently change effort after a provider refusal.

Limits are at most4 MiB per uploaded file,128 calls and300 seconds for the entire
job, further narrowed by the effective policy. A pipeline grant allows33 text
helper calls,32 active embedding batches,8 OCR calls and, when needed,32 desired
embedding batches. Actual dispatch must fit the shared remaining ledger. Fixed
services count bytes/attempts and retain unknown token/cost accounting. No token
or monetary ceiling is invented for a service that cannot report it.

The worker reads a server-created immutable file copy. Queue checkpoints contain
only model references, source metadata/hash, the original actor, ledger and
absolute deadline; provider credentials are resolved at dispatch and never stored
in the checkpoint. Source bytes/metadata, current actor role, KB/project manage
access and the gate are checked again before and during work. Changed bytes,
permissions, index/profile revisions or an expired deadline stop the old job.
Retries do not acquire a new ledger or rerun a started phase automatically.
An explicit new upload is a new authorised operation.

The existing durable tasks scheduler handles the `ingest` job kind. Its payload
contains only `routed_job_id`; the worker matches the actual delivery/task ID and
owns one renewable fenced lease. Duplicate successful delivery is inert and a
different task ID cannot adopt that result. Recovery treats interrupted work as
unknown spend and dead-letters it. Temporary copies are deleted after completion;
terminal cleanup retries on recovery. It deletes only the server-generated job
basename within the canonical document root. No recursive filesystem operation
is introduced.

Document readiness, the `document.ingested` outbox event and job success commit
in one fenced transaction. A stopped or replaced worker cannot publish a ready
document through its old lease. PDF parsing/chunking/parent-child settings are
retained. Native extraction, BM25, dense embedding and Qdrant upsert run in ML;
this integration does not seed vectors or bypass extraction. OCR, hierarchy and
dual-write code paths are bounded but were not live-tested by the V22 text smoke.

ML tags metadata/context calls explicitly and propagates strict routing failures
instead of silently falling back to an unaccounted legacy model. The connected
ingestion task is cancelled when Core disconnects. Both inference reports are
sealed inside the task that owns their ContextVars. Windows extended drive/UNC
paths are resolved before equivalent spellings are normalised; storage escape
checks remain enforced.

V22 exercised actual authenticated upload -> durable Core scheduler -> ML
extraction/chunking -> installed BGE-M3/BM25 -> an initially empty private Qdrant
collection -> authenticated Auto RAG answer/citations. Two published SQuAD source
paragraphs were reused from V19/V20, so this is operational regression, not a new
quality cohort. The real run used10 local generations,7 embedding calls and5
rerank calls, zero cloud, no new model weights and no retries. Qwen/none stayed
ineligible for JSON; the private policy explicitly assigned installed Llama/none
to the structured retrieval helpers and Qwen to the text answer. Those capability
assignments are declared hypotheses, not verified benchmark results.

The final synthetic regression covers gate-off refusal before inference, the
same upload/index/answer path, atomic success and snapshot deletion, completed
duplicate delivery and wrong delivery identity. An earlier zero-egress failure
exposed Windows canonical-path rejection; it is retained in the workspace report.
RAG also refused the initial Qwen-only helper configuration as required by its
JSON compatibility restriction. No output repair or restriction removal was used.

The opt-in `routing_pilot` example has scoped receipt/worker-control endpoints
only in its private router and schema. It registers no periodic jobs; production
HTTP/scheduler entry points and upload authorization remain the exercised path.
Enterprise consumes the same Core implementation and its policy seams; the
installed consumer compiles/tests, but live Enterprise connector-source answer
quality was not established. Independent families/review and P12 admission remain
separate. Read the workspace `report.v22.md` and primary native ledger/receipts.


For the subsequent V26 PDF/DOCX/image OCR, contextual and dual-write checks,
see [ingestion formats and failure preservation](model-routing-ingestion-formats.md).
Historical V22/V25 scope statements above describe those earlier runs.
