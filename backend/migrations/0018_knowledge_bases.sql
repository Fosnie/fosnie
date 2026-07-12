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

-- 0018_knowledge_bases.sql — Libraries (modular, shareable Knowledge Bases).
-- Promotes Project Knowledge to a first-class, standalone entity decoupled from
-- a single Project. Sharing is EXPLICIT
-- linking (project_kb_links / chat_kb_links) + a query-time INTERSECTION
-- allow-list — never inheritance. Supersedes the
-- project-bound model (project_knowledge / knowledge_docs), which are dropped
-- here after the rows are re-keyed (ids preserved, so Qdrant/agent refs survive).
-- Forward-only; owned by sqlx-cli.

CREATE TYPE kb_visibility  AS ENUM ('personal', 'project', 'team', 'shared');
CREATE TYPE kb_permission  AS ENUM ('read', 'manage');  -- manage implies read

-- A Knowledge Base ("Library"): standalone, owned, attachable to many Projects.
CREATE TABLE knowledge_bases (
    id                  UUID          PRIMARY KEY,
    name                TEXT          NOT NULL,
    description         TEXT,
    owner_id            UUID          NOT NULL REFERENCES users(id),
    visibility          kb_visibility NOT NULL DEFAULT 'personal',   -- UI hint; real access is grants
    origin_project_id   UUID          REFERENCES projects(id) ON DELETE SET NULL,  -- set if it began as a Project's default KB
    restricted          BOOLEAN       NOT NULL DEFAULT false,        -- if true, never attachable outside origin (hard wall)
    embedding_model_id  TEXT          NOT NULL,
    embedding_dimension INT           NOT NULL,
    status              pk_status     NOT NULL DEFAULT 'empty',
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT now(),
    last_ingest_at      TIMESTAMPTZ,
    archived_at         TIMESTAMPTZ
);
CREATE INDEX knowledge_bases_owner_idx  ON knowledge_bases (owner_id);
CREATE INDEX knowledge_bases_origin_idx ON knowledge_bases (origin_project_id);

-- ReBAC tuples on the KB. One row per principal (manage implies read), so the
-- UNIQUE has no `permission`.
CREATE TABLE kb_access_grants (
    id             UUID           PRIMARY KEY,
    kb_id          UUID           NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    principal_type principal_type NOT NULL,
    principal_id   UUID           NOT NULL,
    permission     kb_permission  NOT NULL,
    granted_by     UUID           REFERENCES users(id),
    created_at     TIMESTAMPTZ    NOT NULL DEFAULT now(),
    UNIQUE (kb_id, principal_type, principal_id)
);
CREATE INDEX kb_access_grants_kb_idx        ON kb_access_grants (kb_id);
CREATE INDEX kb_access_grants_principal_idx ON kb_access_grants (principal_id);

-- Which KBs a Project sees (explicit, audited attach — the cross-matter surface).
CREATE TABLE project_kb_links (
    project_id  UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kb_id       UUID        NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    attached_by UUID        REFERENCES users(id),
    attached_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, kb_id)
);
CREATE INDEX project_kb_links_kb_idx ON project_kb_links (kb_id);

-- Ad-hoc attach for personal / ungrouped chats.
CREATE TABLE chat_kb_links (
    chat_id     UUID        NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    kb_id       UUID        NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    attached_by UUID        REFERENCES users(id),
    attached_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, kb_id)
);
CREATE INDEX chat_kb_links_kb_idx ON chat_kb_links (kb_id);

-- Documents belong to a KB, not a Project.
CREATE TABLE kb_documents (
    id                UUID        PRIMARY KEY,
    kb_id             UUID        NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    original_filename TEXT        NOT NULL,
    mime              TEXT,
    bytes_path        TEXT        NOT NULL,
    ingest_status     doc_status  NOT NULL DEFAULT 'uploaded',
    created_by        UUID        REFERENCES users(id),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX kb_documents_kb_idx ON kb_documents (kb_id);

-- ---------------------------------------------------------------------------
-- Backfill the existing project-bound model into the new shape (ids preserved).
-- ---------------------------------------------------------------------------

-- Each Project Knowledge → a project-visibility KB with the SAME id.
INSERT INTO knowledge_bases
    (id, name, description, owner_id, visibility, origin_project_id, restricted,
     embedding_model_id, embedding_dimension, status, created_at, last_ingest_at)
SELECT pk.id, p.name, p.description, p.owner_user_id,
       'project'::kb_visibility, pk.project_id, false,
       pk.embedding_model_id, pk.embedding_dimension, pk.status,
       pk.created_at, pk.last_ingest_at
FROM project_knowledge pk
JOIN projects p ON p.id = pk.project_id;

-- Owner → manage on their project KB.
INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by)
SELECT gen_random_uuid(), kb.id, 'user'::principal_type, kb.owner_id, 'manage'::kb_permission, kb.owner_id
FROM knowledge_bases kb;

-- Existing project grantees → read on the project KB (translate the project ACL),
-- skipping the owner (already manage).
INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by)
SELECT DISTINCT gen_random_uuid(), kb.id, g.principal_type, g.principal_id,
       'read'::kb_permission, kb.owner_id
FROM knowledge_bases kb
JOIN access_grants g
  ON g.resource_type = 'project' AND g.resource_id = kb.origin_project_id
WHERE NOT (g.principal_type = 'user' AND g.principal_id = kb.owner_id)
ON CONFLICT (kb_id, principal_type, principal_id) DO NOTHING;

-- Self-link each project KB to its origin Project.
INSERT INTO project_kb_links (project_id, kb_id, attached_by, attached_at)
SELECT kb.origin_project_id, kb.id, kb.owner_id, kb.created_at
FROM knowledge_bases kb
WHERE kb.origin_project_id IS NOT NULL;

-- Re-key documents (same id); force re-ingest into the single collection.
INSERT INTO kb_documents
    (id, kb_id, original_filename, mime, bytes_path, ingest_status, created_by, created_at)
SELECT kd.id, kd.project_knowledge_id, kd.original_filename, kd.mime, kd.bytes_path,
       'uploaded'::doc_status, kd.created_by, kd.created_at
FROM knowledge_docs kd;

-- Repoint FKs (ids preserved ⇒ mechanical).
ALTER TABLE citations DROP CONSTRAINT citations_doc_id_fkey;
ALTER TABLE citations ADD CONSTRAINT citations_doc_id_fkey
    FOREIGN KEY (doc_id) REFERENCES kb_documents(id) ON DELETE SET NULL;

ALTER TABLE agent_project_knowledge
    DROP CONSTRAINT agent_project_knowledge_project_knowledge_id_fkey;
ALTER TABLE agent_project_knowledge
    ADD CONSTRAINT agent_project_knowledge_project_knowledge_id_fkey
    FOREIGN KEY (project_knowledge_id) REFERENCES knowledge_bases(id) ON DELETE CASCADE;

-- Rebuild the single Qdrant collection: one ingest task per document (the
-- scheduler drains these, ensuring pai_kb and stamping knowledge_base_id).
INSERT INTO tasks (id, task_type, payload)
SELECT gen_random_uuid(), 'ingest'::task_type, jsonb_build_object('doc_id', id)
FROM kb_documents;

-- Drop the superseded project-bound tables.
DROP TABLE knowledge_docs;
DROP TABLE project_knowledge;
