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

-- 0008_tabular.sql — Tabular review: matrix of documents × extraction prompts
-- One engine, two presentations
-- (matrix grid + N=1 prose). Per-cell = per-document extraction over a KNOWN
-- SET (never base RAG). Review-scoped chat reuses `chats` via a nullable FK.
-- Forward-only; owned by sqlx-cli.

-- New durable task kind for background cell generation (used at runtime, not here).
ALTER TYPE task_type ADD VALUE 'tabular_generate';

CREATE TYPE tabular_cell_status AS ENUM ('pending', 'running', 'done', 'error');

-- A review: N documents × M extraction columns. `columns_config` is an array of
-- {key, name, format, prompt}; format drives the per-cell prompt suffix + typing.
CREATE TABLE tabular_reviews (
    id             UUID        PRIMARY KEY,
    project_id     UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name           TEXT        NOT NULL,
    columns_config JSONB       NOT NULL,
    status         TEXT        NOT NULL DEFAULT 'pending',  -- pending | running | done | error
    created_by     UUID        REFERENCES users(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The documents under review (workspace documents, version-pinned).
CREATE TABLE tabular_review_documents (
    review_id   UUID NOT NULL REFERENCES tabular_reviews(id) ON DELETE CASCADE,
    document_id UUID NOT NULL REFERENCES documents(id),
    position    INT  NOT NULL DEFAULT 0,
    PRIMARY KEY (review_id, document_id)
);

-- One row per (review, document, column). Citations embedded as JSONB in the
-- unified format (§11.3 permits embedded; the citations table stays message-scoped).
CREATE TABLE tabular_cells (
    id          UUID                PRIMARY KEY,
    review_id   UUID                NOT NULL REFERENCES tabular_reviews(id) ON DELETE CASCADE,
    document_id UUID                NOT NULL REFERENCES documents(id),
    column_key  TEXT                NOT NULL,
    status      tabular_cell_status NOT NULL DEFAULT 'pending',
    value       JSONB,
    reasoning   TEXT,
    citations   JSONB,
    error       TEXT,
    updated_at  TIMESTAMPTZ,
    UNIQUE (review_id, document_id, column_key)
);
CREATE INDEX tabular_cells_review_idx ON tabular_cells (review_id);

-- Review-scoped chat: a normal chat carrying the review it is scoped to.
ALTER TABLE chats ADD COLUMN tabular_review_id UUID REFERENCES tabular_reviews(id);
