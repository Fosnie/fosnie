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

-- 0014_automations.sql — Cron-style automations + run history. A scheduled chat against an
-- Agent; the calendar is a view onto upcoming occurrences. Schedule is a cron
-- expression; next_run_at lives in the DB so scheduling is restart-safe.
-- Forward-only; owned by sqlx-cli.

CREATE TYPE automation_status AS ENUM ('active', 'paused');
CREATE TYPE automation_run_status AS ENUM ('running', 'succeeded', 'failed');

CREATE TABLE automations (
    id            UUID              PRIMARY KEY,
    owner_user_id UUID              NOT NULL REFERENCES users(id),
    name          TEXT              NOT NULL,
    schedule      TEXT              NOT NULL,                 -- cron expression
    prompt        TEXT              NOT NULL,
    agent_id      UUID              REFERENCES agents(id),    -- null = default Agent
    status        automation_status NOT NULL DEFAULT 'active',
    next_run_at   TIMESTAMPTZ,
    last_run_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ       NOT NULL DEFAULT now()
);
CREATE INDEX automations_due_idx ON automations (status, next_run_at);

CREATE TABLE automation_runs (
    id             UUID                  PRIMARY KEY,
    automation_id  UUID                  NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
    status         automation_run_status NOT NULL DEFAULT 'running',
    started_at     TIMESTAMPTZ           NOT NULL DEFAULT now(),
    completed_at   TIMESTAMPTZ,
    output_chat_id UUID                  REFERENCES chats(id),
    error          TEXT,
    created_at     TIMESTAMPTZ           NOT NULL DEFAULT now()
);
CREATE INDEX automation_runs_automation_idx ON automation_runs (automation_id);
