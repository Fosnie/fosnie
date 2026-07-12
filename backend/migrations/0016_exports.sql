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

-- 0016_exports.sql — Async export jobs with a download link.
-- Large exports run as a durable background task instead of blocking
-- the request: a row is queued, the scheduler builds the file to disk, and a
-- download endpoint serves it. The synchronous endpoints remain for small
-- exports. Qdrant vectors are never exported. Forward-only; owned by sqlx-cli.

ALTER TYPE task_type ADD VALUE IF NOT EXISTS 'export';

CREATE TYPE export_kind   AS ENUM ('chat', 'project_db', 'audit');
CREATE TYPE export_status AS ENUM ('queued', 'running', 'ready', 'failed');

CREATE TABLE exports (
    id           UUID          PRIMARY KEY,
    requested_by UUID          NOT NULL REFERENCES users(id),
    kind         export_kind   NOT NULL,
    target_id    UUID,                              -- chat_id / project_id; null for audit
    format       TEXT          NOT NULL DEFAULT 'json',
    status       export_status NOT NULL DEFAULT 'queued',
    disk_path    TEXT,
    mime         TEXT,
    filename     TEXT,
    error        TEXT,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX exports_requester_idx ON exports (requested_by, created_at DESC);
