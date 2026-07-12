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

-- 0010_legal_holds.sql — Legal hold: a no-delete flag that BEATS audit retention
-- Granularity per-project (matter) and per-document. Setting
-- or clearing a hold is itself an audit event. Forward-only; owned by sqlx-cli.

CREATE TABLE legal_holds (
    id            UUID        PRIMARY KEY,
    resource_type TEXT        NOT NULL,            -- 'project' | 'document'
    resource_id   UUID        NOT NULL,
    active        BOOLEAN     NOT NULL DEFAULT true,
    reason        TEXT,
    set_by        UUID        REFERENCES users(id),
    set_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    cleared_by    UUID        REFERENCES users(id),
    cleared_at    TIMESTAMPTZ
);

-- At most one active hold per resource.
CREATE UNIQUE INDEX legal_holds_active_idx
    ON legal_holds (resource_type, resource_id) WHERE active;
