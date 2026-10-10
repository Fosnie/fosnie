# Recoverable index writes and embedding publication

Configured shared document storage now holds `.index-writes` journals and a
process-shared OS file lock. Before replacing a document, ML snapshots its old
points/vectors in active and desired physical collections, shared parents and
the per-document notes cache. The journal is bounded to128MiB/8192points per set
and128pending journals, scoped to exact KB/document IDs and validated collection
names. It stores corpus data, never API keys or inference endpoints. Treat it
with the same access controls and shared-volume lifetime as document storage.

The snapshot file is flushed/fsynced and renamed before the first delete/upsert.
A propagated write error or cancellation restores the old corpus before releasing
the lock. Failed compensation leaves the journal pending; Fosnie storage operations
refuse to serve uncertain data until recovery succeeds. A process crash releases
the OS lock, and startup/next fenced operation restores pending snapshots without
model calls. Successfully completed writes remove their journals. This is crash
compensation, not a distributed transaction or a power-loss durability guarantee.

All Fosnie ML Qdrant helper operations share this fence, including each streamed
batch. The fence holds storage operations, not inference. All replicas serving
the same Qdrant data must use the same configured STORAGE_ROOT/lock-capable volume.
External Qdrant clients and replicas with independent roots are outside this
protocol. Existing unconstrained development mode without a root retains its old
behaviour; opted-in routed Core ingestion explicitly requires storage recovery
and a configured root. Large prior snapshots exceeding bounds fail before writes.
TOC is still an optional derived aid outside document compensation.

Embedding publication persists a PostgreSQL intent before alias mutation, recording
old/new collections and the desired revision captured before build. A lost alias
reply or SQL commit failure leaves the intent pending. Active/desired embedding
dispatch and desired target edits are fenced until model-free recovery confirms
the alias and commits provenance, KB dimensions, adopted current desired profiles
and intent completion together. The scheduler checks pending publication on each
poll. An unexpected alias or changed provenance remains blocked; recovery does not
guess another target or silently rebuild embeddings.

The scheduler also records an embedding rebuild attempt before inference. A
started/interrupted/failed attempt cannot automatically run inference again.
A completed build can resume publication only. An explicit new rebuild owns a
new task/attempt. These legacy rebuild attempt records prevent replay; they are
not a claim that legacy full-corpus rebuild now has a routed policy/token ledger.
Routed ingestion checks active index revision as well as source/actor rights so
same-dimension encoder changes cannot silently validate an old document checkpoint.

ML alias switching is idempotent and retains the old physical collection. The first
literal-to-alias conversion first copies all literal vectors/payloads to a retained
backup and writes a receipt before removing the literal name. An interruption in
the name gap can recreate the captured alias from this receipt. Previous indexes
and literal backups require an explicit retention/cleanup policy; they are no
longer discarded before SQL publication. Alias and PostgreSQL are still two stores;
the pending intent is the recovery protocol, not cross-store atomicity.

V28 uses real temporary Qdrant and Core/Postgres/ML with synthetic vectors, model
calls0. Five storage faults pass: partial dual-index write, unavailable restore
with blocked reads and later repair, os._exit process crash, literal source backup,
and literal/alias-gap recovery. Old vectors, parents, notes and an unrelated document
remain exact; pending document journals0. Five publication checks pass: lost reply
after an actual alias switch, durable pending SQL intent, new inference blocked,
model-free recovery, inert repeated recovery and retained source index. Core's
active profile is adopted from the valid staged desired attestation.

Affected ML71tests and Core68routing tests pass/1ignored; Core/Enterprise targets
compile with unified migration135. No frontend change or repeated frontend suite.
Private services/indexes/schemas are removed afterwards. This stage does not use
or expand any model allocation. Large-corpus snapshot cost, multi-replica volume
behaviour, hardware power loss and prolonged storage outages need operational
validation. P12 semantic quality remains separate. Read workspace report.v28.md,
report.v28-summary.json, fault reports and cleanup receipts.


The later [V29 full-corpus runtime](model-routing-full-reindex.md) routes new rebuild
requests through one fixed-service ledger and adds explicit retired-index cleanup.
The legacy-rebuild limitation above describes the earlier V28 path.
