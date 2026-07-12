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

-- 0007_documents_versions.sql — Legal-workspace documents with version history,
-- provenance and tracked changes. DISTINCT from the unversioned RAG `knowledge_docs`: these
-- are version-pinned, first-class, retained. Forward-only; owned by sqlx-cli.

-- Provenance of a version (documents-versions "Source enum").
CREATE TYPE doc_source AS ENUM (
    'upload', 'user_upload', 'assistant_edit', 'user_accept', 'user_reject', 'generated'
);

-- Tracked-change author kind + lifecycle.
CREATE TYPE edit_author AS ENUM ('human', 'assistant');
CREATE TYPE edit_status AS ENUM ('pending', 'accepted', 'rejected');

-- A first-class workspace document; one per uploaded file, inside a Project.
CREATE TABLE documents (
    id                 UUID        PRIMARY KEY,
    project_id         UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    original_filename  TEXT        NOT NULL,
    mime               TEXT,
    current_version_id UUID,                              -- FK added after document_versions
    created_by         UUID        REFERENCES users(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at         TIMESTAMPTZ                        -- soft-delete
);
CREATE INDEX documents_project_idx ON documents (project_id) WHERE deleted_at IS NULL;

-- Retained version chain; every edit/accept/reject appends a new version.
CREATE TABLE document_versions (
    id             UUID        PRIMARY KEY,
    document_id    UUID        NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    version_number INT         NOT NULL,
    source         doc_source  NOT NULL,
    bytes_path     TEXT        NOT NULL,
    pdf_path       TEXT,                                  -- cached DOCX→PDF rendition
    byte_size      BIGINT,
    created_by     UUID        REFERENCES users(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, version_number)
);
CREATE INDEX document_versions_doc_idx ON document_versions (document_id);

ALTER TABLE documents
    ADD CONSTRAINT documents_current_version_fk
    FOREIGN KEY (current_version_id) REFERENCES document_versions(id);

-- One row per tracked change (schema §4.6). A logical edit carries a single
-- stable `w_id` applied to both its <w:del> and <w:ins> OOXML elements; accept/
-- reject resolves the pair together. Author attribution drives by-author filtering.
CREATE TABLE document_edits (
    id                  UUID        PRIMARY KEY,
    document_id         UUID        NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    document_version_id UUID        NOT NULL REFERENCES document_versions(id),  -- version carrying the ins/del
    w_id                TEXT        NOT NULL,
    author              edit_author NOT NULL,
    author_user_id      UUID        REFERENCES users(id),  -- set when author = 'human'
    find_text           TEXT,
    replace_text        TEXT,
    context_before      TEXT,
    context_after       TEXT,
    status              edit_status NOT NULL DEFAULT 'pending',
    resolved_by         UUID        REFERENCES users(id),
    resolved_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX document_edits_doc_status_idx ON document_edits (document_id, status);
CREATE INDEX document_edits_version_idx ON document_edits (document_version_id);
