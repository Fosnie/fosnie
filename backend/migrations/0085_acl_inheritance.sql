-- Copyright 2026 Private AI Ltd (SC881079)
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.

-- 0085_acl_inheritance.sql — enforcement of connected-source ACLs
-- (Enterprise source-ACL inheritance). Migration 0084
-- captured lossless source ACLs into `source_acl_snapshots`; this migration
-- adds the consumer side: a mapping of source principals → our users/groups, the
-- materialised per-document entitlement set that the read-path seam checks, and the
-- per-mapping enforcement mode. Unified-migrations rule (like 0084): these tables
-- are created by Core but written only by the Enterprise edition — a Core-only
-- deploy carries them inert and the seam defaults keep behaviour byte-identical.
-- Forward-only; owned by sqlx-cli.

-- A source principal (a DMS user/group or a mail owner) seen in a snapshot, and how
-- (if at all) it resolves to one of our principals. `principal_key` is the
-- normalised identity extracted from the raw ACL: 'user:<email|source-id>' or
-- 'group:<external-id|name>'. `status`:
--   unmatched — seen, no mapping yet (D6: grants access to nobody, sits in the queue)
--   auto      — resolved automatically (email / external_id / unique name), see matched_via
--   manual    — an admin mapped it by hand
--   ignored   — an admin declared "never map" (leaves the queue, grants nothing)
-- One row per (kind, principal_key); `kind` is the ConnectorKind so the same textual
-- name in two sources never collides.
CREATE TABLE source_principal_mappings (
    id                    UUID           PRIMARY KEY,
    kind                  TEXT           NOT NULL,       -- ConnectorKind::as_str (imanage|netdocuments|outlook|gmail)
    principal_key         TEXT           NOT NULL,       -- normalised: 'user:<email|id>' | 'group:<external_id|name>'
    principal_display     TEXT,                          -- human label from the source (for the queue)
    mapped_principal_type principal_type NULL,           -- our user|group once resolved
    mapped_principal_id   UUID           NULL,
    status                TEXT           NOT NULL DEFAULT 'unmatched',
    matched_via           TEXT           NULL,           -- email | external_id | name | manual
    updated_by            UUID           REFERENCES users(id),
    updated_at            TIMESTAMPTZ    NOT NULL DEFAULT now(),
    CONSTRAINT source_principal_mappings_key_uniq UNIQUE (kind, principal_key),
    CONSTRAINT source_principal_mappings_status_chk
        CHECK (status IN ('unmatched', 'auto', 'manual', 'ignored')),
    CONSTRAINT source_principal_mappings_matched_via_chk
        CHECK (matched_via IS NULL OR matched_via IN ('email', 'external_id', 'name', 'manual')),
    -- A resolved mapping (auto|manual) must name a target; unmatched|ignored must not.
    CONSTRAINT source_principal_mappings_resolution_chk CHECK (
        (status IN ('auto', 'manual')  AND mapped_principal_type IS NOT NULL AND mapped_principal_id IS NOT NULL)
     OR (status IN ('unmatched', 'ignored') AND mapped_principal_type IS NULL AND mapped_principal_id IS NULL)
    )
);
CREATE INDEX source_principal_mappings_status_idx ON source_principal_mappings (status);
CREATE INDEX source_principal_mappings_target_idx
    ON source_principal_mappings (mapped_principal_type, mapped_principal_id)
    WHERE mapped_principal_id IS NOT NULL;

-- Materialised entitlement: "this (our) principal may read this document, because a
-- mapped source principal with read-rights is in its latest snapshot" (D4). Group
-- membership is NOT expanded here — the group principal is stored and joined against
-- `group_members` at check time, so a SCIM membership change takes effect with no
-- recompute (D4 / §4). Only built for documents under a warn|enforce mapping.
CREATE TABLE document_entitlements (
    document_id    UUID           NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    principal_type principal_type NOT NULL,
    principal_id   UUID           NOT NULL,
    PRIMARY KEY (document_id, principal_type, principal_id)
);
-- Hot path: "which documents may this principal read?" (the batch filter_documents).
CREATE INDEX document_entitlements_principal_idx
    ON document_entitlements (principal_type, principal_id);

-- Per-mapping enforcement mode (D3): off = today's behaviour (no ACL check);
-- warn = everything visible but the UI badges "restricted at source" + reports;
-- enforce = real deny (non-entitled see 404 / doc absent from lists). New mappings
-- default to `warn` so enabling the feature is non-destructive (D3).
ALTER TABLE connector_sync_mappings
    ADD COLUMN acl_mode TEXT NOT NULL DEFAULT 'warn'
        CONSTRAINT connector_sync_mappings_acl_mode_chk
        CHECK (acl_mode IN ('off', 'warn', 'enforce'));

-- Per-item ACL health, denormalised for the badge/report without recomputing on the
-- fly (D6): ok = every reader mapped; unmapped_principals = ≥1 reader unmatched;
-- no_snapshot = hand-uploaded / no source ACL (unrestricted, project access governs).
ALTER TABLE connector_items
    ADD COLUMN acl_status TEXT NOT NULL DEFAULT 'ok'
        CONSTRAINT connector_items_acl_status_chk
        CHECK (acl_status IN ('ok', 'unmapped_principals', 'no_snapshot'));

-- Derived reader-principal set extracted from the raw `acl` at capture time, so the
-- materialiser never re-parses the source-specific ACL shape (§4). Array of the same
-- normalised `principal_key` strings used by source_principal_mappings. A GIN index
-- powers the reverse lookup "which snapshots contain this principal_key?" used to
-- recompute the docs affected by a single mapping change (§4 incremental).
ALTER TABLE source_acl_snapshots
    ADD COLUMN readers JSONB;
CREATE INDEX source_acl_snapshots_readers_idx
    ON source_acl_snapshots USING GIN (readers);
