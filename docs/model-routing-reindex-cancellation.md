# Cancelling an embedding rebuild and starting a fresh attempt

The provider admin screen offers **Cancel my re-index** through POST
`/api/admin/embedding-index/cancel`. It requires `providers.manage`, validates the
current principal and selects only that caller's queued/running routed reindex
whose durable attempt is still `building`. One transaction cancels the routed
job, clears its lease, marks the attempt failed, ends its queued/running task and
records `routing.reindex.cancelled`. A request with no eligible owned job returns
`cancelled: null`. Another user's job cannot be selected. Once the built checkpoint
owns publication, cancellation is unavailable; model-free publication recovery
continues to resolve the existing intent.

Cancellation preserves the consumed ledger and attempt. It cannot recall an
upstream request already sent. The worker monitor and lease fences stop further
Core work; missing fixed-service usage remains conservatively charged against its
reserved grant rather than being refunded. The V30 interrupted run therefore
retains `calls: 128` / unknown usage for its original reservation, although the
native request ledger proves only one request was forwarded for that run. These
are separate accounting quantities, not 128 actual provider calls.

The active corpus stays published while a partial desired collection is retained
unpublished. **Start new attempt** creates new task/run identifiers, a new bounded
ledger and deadline, and explicitly rebuilds the unaliased desired target. It
never resumes the cancelled inference allowance. Completed duplicate delivery is
inert; cancelled or wrong-task delivery cannot start inference. A stale worker
cannot change terminal/index lifecycle state after losing ownership. Failure
updates also exclude a published index and any newer routed rebuild; index revision
checks and pending publication fences remain in force.

V30 exercised actual Core queue/cancel endpoints, durable scheduler, ML and Qdrant
with 130 test chunks. Synthetic and native Qwen3-Embedding runs both passed:

- Cancellation before worker start: no model reservation or native call.
- Cancellation after the first 64-text batch: the next request was held before
  upstream, cancelled and never forwarded; the old corpus stayed active.
- Redelivery of the cancelled task: refused, no inference.
- Explicit fresh attempt: three batches of 64/64/2, one fixed-only ledger with
  three calls, desired corpus published and source retained.
- Completed duplicate and wrong-task delivery: no new model request.

Native evidence contains five proxy reservations, four real embedding calls
(all HTTP 200), zero classifier/cloud calls and a 1024-dimensional vector probe.
The source vectors are synthetic and paragraph text is reused public regression
data; this proves operational behaviour, not semantic quality or P12 admission.
The held-request boundary does not prove that an already-sent provider request can
be stopped. Large corpus throughput, simultaneous workers, corpus/ACL drift and
independent quality admission still require separate evidence.

Core routing tests (68 passed, one ignored), Core/Enterprise targets and frontend
types passed in the affected verification block. After a final stale-worker fence
repair, the private consumer was rebuilt and the entire synthetic protocol passed
again in a new exclusive allocation with zero real model calls. The native claim
was preserved and was not replayed. Unchanged ML tests were not repeated.

Read workspace `report.v30.md`, `report.v30-summary.json`, native ledger and final
synthetic receipt. Private schemas/apps/services/indexes were removed; the owned
model server was stopped and the existing V27 model cache was retained. No global
model store or production deployment was changed.

V31 adds [shared-root builder locks and publication drift checks](model-routing-reindex-fences.md), with a two-process ML protocol.
