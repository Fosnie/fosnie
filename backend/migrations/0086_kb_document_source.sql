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

-- 0086_kb_document_source.sql — provenance for KB documents.
-- Until now every kb_documents row came from a manual
-- upload; a connector import can now land a document straight into a KB (the RAG
-- corpus that grounds chats). `source` records which path created the row so the
-- library UI can badge connector-imported documents and the sync branch can tell
-- its own rows apart from hand-uploaded ones. Default 'upload' keeps every
-- existing row (and the plain upload handler) byte-identical. Forward-only.
ALTER TABLE kb_documents
    ADD COLUMN source TEXT NOT NULL DEFAULT 'upload'
        CONSTRAINT kb_documents_source_chk
        CHECK (source IN ('upload', 'connector_import'));
