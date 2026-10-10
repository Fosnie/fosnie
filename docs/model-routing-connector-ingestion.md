# Routed connector document ingestion

Core owns the opt-in `routing.connector_ingestion_enabled` gate, durable ingestion
runtime and ledger. It is separate from human uploads (`routing.ingestion_enabled`)
and defaults off. A providers manager can GET/PUT `/api/routing/connector-ingestion`
or use the Routing settings control. GET reports state; PUT requires a current
principal and `providers.manage`. Opening a gate does not migrate or replay jobs.

Connector importers store bytes through `kb::stage_bytes`/`replace_bytes_staged`,
capture provenance and ACLs, then call `ingestion::queue_connector` with their
original AuthContext. Actorless refresh cannot enqueue inference under this gate;
older legacy connector tasks are refused before ML dispatch while it is enabled.
With the connector gate off, staged imports retain legacy indexing after capture.

An installed RbacPolicy extension must implement `connector_ingestion_witness`.
Its Core default denies; Core assumes neither access from KB management alone nor
an admin/system model principal. The Enterprise implementation requires an active
source connection, one non-deleted provenance item, the latest captured reader
set, its current user/group mapping, a materialised matching entitlement and live
group membership. Warn/off and admin override do not bypass this ingestion check.
No raw ACL, source URL or credential enters the routing checkpoint: only a hashed
revision witness. ACL changes here mean changes captured in Fosnie; immediate
uncaptured remote vendor changes require another source refresh.

Unmapped/denied or misconfigured imports remain stored with ingest status `error`
and an audited `routing.connector_ingestion.blocked` outcome. No model job or
content forwarding occurs for a denied source. Fixing ACL mapping does not enqueue
inference automatically. A manager of the Library with current source access can
explicitly POST `/api/routing/connector-ingestion/{document_id}/queue` (no body),
or select **Reindex** beside the imported document in Library. The button appears
only while the gate is enabled and the document is in a terminal state. Queueing
updates the document to `uploaded` in the same transaction as its task/job insert.
An active routed job returns409; authorisation fails before planning when denied.
Concurrent starts are serialised at insertion by the document row lock.

Manual imports use the importer; scheduled sync loads the current connection
owner through `auth::load_context`. It does not manufacture an administrator.
The durable job retains this role/user and its effective Auto policy, selected
text helpers, pinned active/desired embedding and OCR references, shared limits,
absolute deadline and immutable bounded snapshot. Same-version re-import refreshes
rights without queueing another ingestion job. A new content version queues a new
snapshot/allocation. The `document.ingested` event preserves Human vs System
origin; a System event has no human actor even though inference is authorised
against its actual sync owner.

The common bounds remain4 MiB,128 calls and300 seconds intersected with policy.
Access/witness is checked at enqueue, worker entry, just before ML dispatch,
every2 seconds during the pipeline and before fenced publication. Source/version,
actor, membership, connection or gate changes stop the old job; already observed
or unknown spend is retained. This is cancellation with a polling interval, not
a claim that an in-flight provider request can be undone. Readiness/outbox/job
success commit together. Successful duplicate delivery is inert; interrupted
work is not replayed. An explicit reindex owns a new allocation. Temporary
snapshots are removed after terminal processing and retried only for cleanup.

V25 uses the actual installed Enterprise REST importer, ACL mapper/materialiser,
sync function, Core scheduler, ML extraction/chunking/BM25/dense index and Auto
ticket/WebSocket answer path. Its task-owned source serves reused public SQuAD
text with a synthetic bearer. A new empty Qdrant collection receives only the
authorised document via worker indexing; seeded vectors0. Unknown principals
produce0 jobs/egress; mapping alone produces0 jobs; membership revocation before
worker launch produces0 upstream calls. The allowed version and a sync version2
succeed; an in-flight metadata reservation is cancelled before upstream when
membership is revoked. Remote ACL-only re-import blocks cited history at0 model
reservations. Signed model provenance and audit chain are verified.

Live upstreams20: Qwen3, Llama7, BGE-M3 embedding6, rerank4; allHTTP200, cloud0.
One additional held reservation was not sent and its unknown backend allowance
was not refunded. Source GETs22: metadata5, downloads6, security10, changes1.
The final UI/queued-state/System-null-actor changes are checked by a final full
synthetic protocol and TypeScript; the live run predates those small final changes.
Permanent connector regressions3pass and affected Enterprise targets compile.

This proves operational routing/accounting/source boundaries. It is not an
independent quality cohort, vendor tenant/OAuth/pagination validation, billing
verification or P12 admission. OCR, PDF/DOCX contextual chunking and dual-write
remain outside this text-only live run. Read workspace report.v25.md/JSON and the
native ledger for exact outcomes. Production settings and real accounts stay
untouched; all new model allocations are bounded and old claims remain intact.


For the subsequent V26 PDF/DOCX/image OCR, contextual and dual-write checks,
see [ingestion formats and failure preservation](model-routing-ingestion-formats.md).
Historical V22/V25 scope statements above describe those earlier runs.
