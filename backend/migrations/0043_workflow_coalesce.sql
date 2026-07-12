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

-- 0043_workflow_coalesce.sql — Coalescing buffer for the workflow engine
-- (the thundering-herd lever). N events
-- of one workflow + scope within the window collapse into ONE run over the batch
-- (50 dropped files → 1 run, not 50). Fixed (tumbling) window from the first
-- event → bounded latency. PK (workflow_id, scope_key) is the accumulation bucket.
-- Forward-only; owned by sqlx-cli.

CREATE TABLE workflow_coalesce (
    workflow_id UUID        NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    scope_key   TEXT        NOT NULL,                 -- project_id::text or 'global'
    event_ids   UUID[]      NOT NULL DEFAULT '{}',
    depth       INT         NOT NULL DEFAULT 0,       -- max depth among buffered events
    fire_at     TIMESTAMPTZ NOT NULL,                 -- window close = first_event + window
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workflow_id, scope_key)
);
CREATE INDEX workflow_coalesce_fire_idx ON workflow_coalesce (fire_at);
