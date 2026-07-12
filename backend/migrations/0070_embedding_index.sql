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

-- 0070_embedding_index.sql — provenance of the active embedding index
-- Single row (id = 1). Records the model/dim/collection
-- that ACTUALLY built the live vectors, so retrieval + ingest embed with a model
-- consistent with the index — independent of the "desired" provider_configs embed
-- row. A model change stages `desired_*` + a warn-gate; a blue-green re-index job
-- rebuilds into a new collection and only then promotes desired → active. API keys
-- are AES-256-GCM ciphertext (crypto.rs), never plaintext.

CREATE TABLE embedding_index (
    id                        INT         PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    -- Active index (what the live vectors were built with).
    embed_model               TEXT        NOT NULL,
    embed_base_url            TEXT,
    embed_api_key_encrypted   TEXT,
    dim                       INT         NOT NULL,
    collection_name           TEXT        NOT NULL DEFAULT 'pai_kb',
    -- Migration state: active | reindexing | failed.
    status                    TEXT        NOT NULL DEFAULT 'active',
    reindex_done              INT         NOT NULL DEFAULT 0,
    reindex_total             INT         NOT NULL DEFAULT 0,
    error                     TEXT,
    -- Desired target (set on an embed-model change; promoted on a successful re-index).
    desired_model             TEXT,
    desired_base_url          TEXT,
    desired_api_key_encrypted TEXT,
    desired_dim               INT,
    desired_collection_name   TEXT,
    built_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by                UUID        REFERENCES users(id),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
