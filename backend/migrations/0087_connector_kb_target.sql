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

-- 0087_connector_kb_target.sql — route connector imports into a Knowledge Base
-- Migration 0084 could only import into a
-- project workspace; this migration lets a sync mapping (and a manual import)
-- target a KB — the RAG corpus — so imported documents actually ground chats.
-- A connector_item now points at EITHER a workspace document or a KB document
-- (a `both` import writes one row of each). Unified-migrations rule (like 0084 /
-- 0085): created by Core, written only by the Enterprise import branch; a
-- Core-only deploy carries these inert and behaviour stays byte-identical.
-- Forward-only.

-- --- Destination on the sync mapping (D1) ----------------------------------
-- workspace = today's behaviour (versioned workspace doc, write-back);
-- kb        = ingest into `target_kb_id` (read corpus, not versioned — D2);
-- both      = two independent copies (D1). A non-workspace destination must name
-- a target KB.
ALTER TABLE connector_sync_mappings
    ADD COLUMN destination TEXT NOT NULL DEFAULT 'workspace'
        CONSTRAINT connector_sync_mappings_destination_chk
        CHECK (destination IN ('workspace', 'kb', 'both')),
    ADD COLUMN target_kb_id UUID NULL REFERENCES knowledge_bases(id) ON DELETE SET NULL,
    ADD CONSTRAINT connector_sync_mappings_target_kb_chk
        CHECK (destination = 'workspace' OR target_kb_id IS NOT NULL);

-- --- A connector item may target a workspace doc OR a KB doc (D5) -----------
-- `document_id` becomes nullable; a new `kb_document_id` carries the KB copy.
-- `parent_item_id` records the email→attachment link when the target is a KB
-- (kb_documents have no parent-child column, so the relationship lives here — D6).
ALTER TABLE connector_items
    ALTER COLUMN document_id DROP NOT NULL;
ALTER TABLE connector_items
    ADD COLUMN kb_document_id UUID NULL REFERENCES kb_documents(id) ON DELETE CASCADE,
    ADD COLUMN parent_item_id UUID NULL REFERENCES connector_items(id) ON DELETE SET NULL,
    ADD CONSTRAINT connector_items_target_chk
        CHECK (document_id IS NOT NULL OR kb_document_id IS NOT NULL);

-- Replace the single dedup key with one partial unique index per target column,
-- so dedup semantics (a given remote_id imported once per destination copy) hold
-- independently for the workspace and KB copies of a `both` import.
ALTER TABLE connector_items DROP CONSTRAINT connector_items_dedup_uniq;
CREATE UNIQUE INDEX connector_items_dedup_ws_uniq
    ON connector_items (kind, remote_id, document_id)
    WHERE document_id IS NOT NULL;
CREATE UNIQUE INDEX connector_items_dedup_kb_uniq
    ON connector_items (kind, remote_id, kb_document_id)
    WHERE kb_document_id IS NOT NULL;
CREATE INDEX connector_items_kb_document_idx ON connector_items (kb_document_id)
    WHERE kb_document_id IS NOT NULL;

-- --- KB-document entitlements (D7) -----------------------------------------
-- The KB-document twin of `document_entitlements` (0085). Same semantics — a
-- mapped source principal with read-rights in the item's latest snapshot; group
-- membership joined live at check time, not expanded here — but keyed on a
-- kb_documents id. A parallel table (rather than a nullable column on
-- document_entitlements) avoids churning that table's primary key. Built only
-- for KB documents under a warn|enforce mapping; consumed by the retrieval-filter
-- seam to build the per-query Qdrant deny-list.
CREATE TABLE kb_document_entitlements (
    kb_document_id UUID           NOT NULL REFERENCES kb_documents(id) ON DELETE CASCADE,
    principal_type principal_type NOT NULL,
    principal_id   UUID           NOT NULL,
    PRIMARY KEY (kb_document_id, principal_type, principal_id)
);
-- Hot path: "which KB documents may this principal read?" (deny-list build).
CREATE INDEX kb_document_entitlements_principal_idx
    ON kb_document_entitlements (principal_type, principal_id);
