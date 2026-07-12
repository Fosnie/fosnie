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

-- 0084_connector_schema.sql — storage for the Enterprise DMS/mail connectors:
-- per-user OAuth connections, deployment app-configs,
-- project↔container sync mappings, import provenance + dedup, and lossless source
-- ACL snapshots. Unified-migrations rule: these tables are created by Core but
-- written only by the Enterprise edition (like the SCIM tables) — a Core-only
-- deploy carries them inert. Secrets live only in the `*_enc` columns (AES-256-GCM
-- with the deployment message key, like dm_bodies / mcp auth); plaintext never in
-- the DB. Forward-only; owned by sqlx-cli.

-- Provenance value for a version imported from an external connector (D4: import =
-- a copy through the normal ingestion pipeline). Added but not used in this
-- migration, so the enum value is safe to introduce inside the migration tx.
ALTER TYPE doc_source ADD VALUE IF NOT EXISTS 'connector_import';

-- Generic document lineage: an attachment document points at its parent (an email
-- rendered to Markdown, D7). Nullable; SET NULL on parent delete keeps the child.
-- Reusable beyond mail (any future parent/child document relation).
ALTER TABLE documents
    ADD COLUMN parent_document_id UUID NULL REFERENCES documents(id) ON DELETE SET NULL;
CREATE INDEX documents_parent_idx ON documents (parent_document_id)
    WHERE parent_document_id IS NOT NULL;

-- Deployment-level app registration per connector kind (D2: the OAuth client is
-- always the customer's — client_id/base_url/tenant/region in `config`, the client
-- secret encrypted). One row per kind; managed under permission `integrations.manage`.
CREATE TABLE connector_app_configs (
    kind              TEXT        PRIMARY KEY,           -- ConnectorKind::as_str (imanage|netdocuments|outlook|gmail)
    config            JSONB       NOT NULL DEFAULT '{}', -- base_url|tenant|region|client_id|…
    client_secret_enc TEXT,                              -- encrypted OAuth client secret
    updated_by        UUID        REFERENCES users(id),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One authenticated connection to a source. `user_id` NULL ⇒ an org (application-
-- permissions) connection for unattended sync of a shared mailbox (D1). Tokens are
-- stored encrypted; `status` drives the reauth badge and pauses sync.
CREATE TABLE connector_connections (
    id                UUID        PRIMARY KEY,
    kind              TEXT        NOT NULL,
    user_id           UUID        REFERENCES users(id) ON DELETE CASCADE,  -- NULL = org connection
    display_name      TEXT        NOT NULL,              -- e.g. the account email / vault
    access_token_enc  TEXT,
    refresh_token_enc TEXT,
    expires_at        TIMESTAMPTZ,
    scopes            TEXT[]      NOT NULL DEFAULT '{}',
    status            TEXT        NOT NULL DEFAULT 'active',  -- active | reauth_required | revoked
    created_by        UUID        REFERENCES users(id),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at      TIMESTAMPTZ,
    CONSTRAINT connector_connections_status_chk
        CHECK (status IN ('active', 'reauth_required', 'revoked'))
);
-- A given account is one connection per (kind, user, display). Two partial uniques
-- so the org form (user_id NULL) is also deduplicated (NULLs are distinct otherwise).
CREATE UNIQUE INDEX connector_connections_user_uniq
    ON connector_connections (kind, user_id, display_name) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX connector_connections_org_uniq
    ON connector_connections (kind, display_name) WHERE user_id IS NULL;
CREATE INDEX connector_connections_user_idx ON connector_connections (user_id);

-- A continuous-sync binding: a remote container (workspace/folder/label) → a project,
-- with a delta cursor and backoff state. `direction` is import_only in v1.
CREATE TABLE connector_sync_mappings (
    id                    UUID        PRIMARY KEY,
    project_id            UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    connection_id         UUID        NOT NULL REFERENCES connector_connections(id) ON DELETE CASCADE,
    kind                  TEXT        NOT NULL,
    remote_container_id   TEXT        NOT NULL,
    remote_container_name TEXT,
    direction             TEXT        NOT NULL DEFAULT 'import_only',
    cursor                TEXT,                            -- Graph deltaLink | Gmail historyId | DMS edit-date+page
    sync_enabled          BOOLEAN     NOT NULL DEFAULT true,
    last_sync_at          TIMESTAMPTZ,
    last_error            TEXT,
    backoff_until         TIMESTAMPTZ,                     -- exponential backoff gate after failures
    created_by            UUID        REFERENCES users(id),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX connector_sync_mappings_project_idx ON connector_sync_mappings (project_id);
CREATE INDEX connector_sync_mappings_conn_idx ON connector_sync_mappings (connection_id);
CREATE INDEX connector_sync_mappings_active_idx ON connector_sync_mappings (sync_enabled)
    WHERE sync_enabled;

-- Import provenance + dedup. One row links an imported document to its source item;
-- re-importing the same remote_id into the same document appends a version (dedup on
-- the unique key). `remote_deleted_at` marks a source deletion (copy is retained, D4);
-- `unsupported_format` flags an attachment stored as raw bytes only (D7).
CREATE TABLE connector_items (
    id                 UUID        PRIMARY KEY,
    mapping_id         UUID        REFERENCES connector_sync_mappings(id) ON DELETE SET NULL,
    document_id        UUID        NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    kind               TEXT        NOT NULL,
    remote_id          TEXT        NOT NULL,
    remote_version     TEXT,
    remote_modified_at TIMESTAMPTZ,
    remote_deleted_at  TIMESTAMPTZ,
    unsupported_format BOOLEAN     NOT NULL DEFAULT false,
    metadata           JSONB       NOT NULL DEFAULT '{}',  -- source profile/custom attributes (provenance)
    imported_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    imported_by        UUID        REFERENCES users(id),
    CONSTRAINT connector_items_dedup_uniq UNIQUE (kind, remote_id, document_id)
);
CREATE INDEX connector_items_document_idx ON connector_items (document_id);
CREATE INDEX connector_items_mapping_idx ON connector_items (mapping_id);
CREATE INDEX connector_items_remote_idx ON connector_items (kind, remote_id);

-- Lossless effective-ACL capture at import/sync time. Nothing is enforced now;
-- Enterprise later maps source principals → our users/groups. Storing it
-- now avoids a full re-sync of the corpus later.
CREATE TABLE source_acl_snapshots (
    id                UUID        PRIMARY KEY,
    connector_item_id UUID        NOT NULL REFERENCES connector_items(id) ON DELETE CASCADE,
    captured_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    acl               JSONB       NOT NULL  -- principals, rights, inheritance flags, source-group names
);
CREATE INDEX source_acl_snapshots_item_idx ON source_acl_snapshots (connector_item_id);
