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

-- 0004_rag.sql — projects, Project Knowledge, knowledge docs, citations
-- (schema §4, §5.3). Enables the RAG layer. Forward-only; owned by sqlx-cli.
--
-- Projects are FLAT (§B.13). One Project Knowledge per Project. PK is NOT
-- versioned (re-index replaces vectors), so no knowledge_doc_versions in 4a.

CREATE TYPE project_sector AS ENUM ('general', 'legal');
CREATE TYPE pk_status AS ENUM ('empty', 'indexing', 'ready', 'error');
CREATE TYPE doc_status AS ENUM ('uploaded', 'extracting', 'indexing', 'ready', 'error');

-- A Project = a matter; flat container for chats, files, knowledge.
CREATE TABLE projects (
    id                  UUID           PRIMARY KEY,
    name                TEXT           NOT NULL,
    description         TEXT,
    owner_user_id       UUID           NOT NULL REFERENCES users(id),
    sector              project_sector NOT NULL DEFAULT 'general',
    client_matter_number TEXT,                          -- legal-only, nullable
    created_at          TIMESTAMPTZ    NOT NULL DEFAULT now(),
    archived_at         TIMESTAMPTZ
);
CREATE INDEX projects_owner_idx ON projects (owner_user_id);

-- One Project Knowledge per Project (the RAG base; one Qdrant collection).
CREATE TABLE project_knowledge (
    id                 UUID        PRIMARY KEY,
    project_id         UUID        NOT NULL UNIQUE REFERENCES projects(id) ON DELETE CASCADE,
    embedding_model_id TEXT        NOT NULL,
    embedding_dimension INT        NOT NULL,            -- Matryoshka size, fixed at create
    status             pk_status   NOT NULL DEFAULT 'empty',
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    last_ingest_at     TIMESTAMPTZ
);

-- A document uploaded into a Project Knowledge base.
CREATE TABLE knowledge_docs (
    id                   UUID        PRIMARY KEY,
    project_knowledge_id UUID        NOT NULL REFERENCES project_knowledge(id) ON DELETE CASCADE,
    original_filename    TEXT        NOT NULL,
    mime                 TEXT,
    bytes_path           TEXT        NOT NULL,
    status               doc_status  NOT NULL DEFAULT 'uploaded',
    created_by           UUID        REFERENCES users(id),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX knowledge_docs_pk_idx ON knowledge_docs (project_knowledge_id);

-- Unified citation contract. doc_version_id is ALWAYS NULL for
-- Project Knowledge (unversioned); populated only for legal-workspace docs later.
CREATE TABLE citations (
    id                  UUID        PRIMARY KEY,
    message_id          UUID        NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    doc_id              UUID        REFERENCES knowledge_docs(id) ON DELETE SET NULL,
    doc_version_id      UUID,                            -- NULL for RAG
    page_number         INT,
    paragraph_anchor    TEXT,
    clause_section_ref  TEXT,
    quote_text          TEXT        NOT NULL,
    additional_metadata JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX citations_message_idx ON citations (message_id);

-- chats.project_id was laid in nullable without an FK (0003); add it now.
ALTER TABLE chats
    ADD CONSTRAINT chats_project_id_fkey
    FOREIGN KEY (project_id) REFERENCES projects(id);
