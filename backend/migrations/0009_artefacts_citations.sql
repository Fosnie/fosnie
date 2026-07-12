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

-- 0009_artefacts_citations.sql — Generated artefacts + citation contract
-- unification so legal-workspace citations carry
-- document_id + version_id. Forward-only; owned by sqlx-cli.

-- Chat-scoped downloadable artefacts (DOCX / PDF / MD), file-on-disk + pointer row.
CREATE TYPE artefact_kind AS ENUM ('docx', 'pdf', 'md');

CREATE TABLE generated_artefacts (
    id         UUID          PRIMARY KEY,
    chat_id    UUID          NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    kind       artefact_kind NOT NULL,
    title      TEXT          NOT NULL,
    disk_path  TEXT          NOT NULL,
    mime       TEXT          NOT NULL,
    created_by UUID          REFERENCES users(id),
    created_at TIMESTAMPTZ   NOT NULL DEFAULT now()
);
CREATE INDEX generated_artefacts_chat_idx ON generated_artefacts (chat_id);

-- Unified citation contract: a citation is EITHER Project-Knowledge/RAG
-- (doc_id → knowledge_docs, doc_version_id NULL — unversioned base) OR
-- legal-workspace (document_id → documents + doc_version_id → document_versions,
-- version-pinned). The only difference between surfaces is the version pin.
ALTER TABLE citations ADD COLUMN document_id UUID REFERENCES documents(id);
ALTER TABLE citations
    ADD CONSTRAINT citations_doc_version_fk
    FOREIGN KEY (doc_version_id) REFERENCES document_versions(id);
