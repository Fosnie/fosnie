# Rebuild concurrency and publication drift fences

Routed full reindex now keeps a bounded, coherent source snapshot under the shared
storage fence. It releases that fence before inference. The source receipt hashes
sorted point IDs and complete payloads; the existing 32768-point / 64 MiB bounds
still apply. Core validates and saves this receipt at the built checkpoint.

A separate OS lock keyed by the physical desired collection spans the entire ML
build, including inference. It uses the shared STORAGE_ROOT and refuses a second
builder before collection reset or inference. Ordinary corpus reads/replacements
use the storage lock and can proceed while embedding is running. Closing the
stream releases the builder lock explicitly; process death releases the OS lock.
A failed/consumed request still requires an explicit new job, rather than inference
replay. Strict routed rebuilds require a configured shared root. Independent roots
and external Qdrant writers do not participate in these locks.

Migration 137 stores source receipts on attempts and publication intents, links
each routed intent to its exact original job and permits a terminal failed intent.
The publication constructor checks the original task's built checkpoint, preventing
stale recovery from adopting a later attempt. Queue admission also excludes any
built attempt still awaiting publication. Settlement updates only the job/attempt
owned by that intent, even after several failed attempts for the same revision.

Core rechecks the original principal, current role, providers.manage, complete live
KB/document scope and installed RbacPolicy immediately before publication dispatch
and each model-free retry. ML holds the target lock and shared storage fence while
checking the actual alias and comparing both source and desired payload digests to
the durable receipt, then switches the alias under the same fence. Thus an edit
after `built` but before the alias switch prevents publication. A document writer
also rechecks its captured active alias after acquiring its replacement fence,
before snapshot or mutation; it cannot silently write into a retired source after
waiting for a concurrent publication.

Only ML's explicit pre-swap 409 `embedding_publication_refused` releases a failed
intent, fails its attempt/job and leaves the old alias serving the corpus. The
consumed inference ledger is retained; a fresh explicit attempt can capture new
data/rights. Busy storage, transport errors and unproven replies keep the existing
pending intent for model-free recovery. If the alias already points at the new
collection, recovery confirms that fact even if data/rights subsequently changed,
so SQL provenance can settle a previously completed switch without inference.

This is not an atomic transaction across RBAC changes and Qdrant. An already
authorised publication RPC cannot be recalled by a subsequent rights change. Legacy
non-routed maintenance keeps its prior hash-optional contract. Migrated routed
pending intents without a source receipt can confirm an existing switch; a source
that has not switched is refused safely. There is no semantic quality/encoder
attestation inferred from the payload digest.

V31 actual Core API/scheduler/ML/Postgres/Qdrant protocol passed six scenarios:

- Source edit during embedding stops publication; redelivery cannot re-infer.
- Document denial during embedding stops the worker; a new queue returns 403.
- User deactivation before a pending publication retry refuses that switch.
- The RbacPolicy document denial seam before retry likewise refuses publication.
- An edit after the built checkpoint is refused under the ML publication fence.
- A fresh attempt succeeds after the failures. A competing Core delivery is
  refused, another queue returns 409, and a second real ML process sharing the
  root refuses the same physical target before reset or inference. The final
  corpus contains 130 points, the source remains retained and duplicate completed
  delivery is inert.

All embeddings in V31 were synthetic: 17 reservations, 16 synthetic responses,
one held request cancelled before response, zero real model/classifier/cloud calls.
The successful new job has three fixed calls (64/64/2), with an empty LLM assignment
map. The interrupted grant retains a conservative 128-call reservation, separately
from request counts. These are operational regression inputs, not P12 evidence.

Affected verification: 119 ML tests passed after fixing three legacy layer doubles
that lacked alias/metadata mocks; 68 Core routing tests passed, one ignored; final
Core/Enterprise targets and private consumer compiled. The final document write
alias guard has its own storage-fence test; no extra inference run was needed.
Frontend code was unchanged and its previous type check was not repeated.

Read workspace report.v31.md, JSON summary, failed and final exclusive receipts.
Private Core/two ML processes, schemas, services and test indexes were removed.
No model server was started, global model/cache state and old claims were preserved.
Large corpus throughput, distributed deployment load, external writers and
independent model quality admission remain separate work.
