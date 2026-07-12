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

-- 0041_events_workflows.sql — Event-driven workflow engine foundation
-- The third way to fire the platform's
-- Agent: an internal domain event. `events` is a transactional outbox — a row is
-- written in the SAME transaction as the mutation it records, so an event exists
-- iff its mutation committed. `workflows` is the definition surface
-- (WHEN trigger [IF condition] THEN action); `workflow_runs` is each firing,
-- deduped by (workflow, trigger event-set) for idempotency (§7a.6, §12.8).
-- Forward-only; owned by sqlx-cli.

CREATE TYPE event_actor_type    AS ENUM ('human', 'agent', 'workflow', 'system');
CREATE TYPE workflow_run_status AS ENUM ('queued', 'running', 'succeeded', 'failed', 'skipped');

-- Durable outbox + audit-adjacent domain log. `dispatched_at` is the relay
-- marker: NULL = not yet seen by the dispatcher (cheap partial-index poll).
CREATE TABLE events (
    id            UUID             PRIMARY KEY,
    event_type    TEXT             NOT NULL,
    actor_type    event_actor_type NOT NULL,
    actor_user_id UUID             REFERENCES users(id),
    resource_type TEXT,
    resource_id   UUID,
    project_id    UUID             REFERENCES projects(id),
    payload       JSONB            NOT NULL DEFAULT '{}'::jsonb,
    causation_id  UUID             REFERENCES events(id),  -- the event/run that caused this one
    trigger_chain UUID[]           NOT NULL DEFAULT '{}',  -- ordered workflow_run ids in this lineage
    depth         INT              NOT NULL DEFAULT 0,     -- workflow hops deep (loop guard, §7a)
    dispatched_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ      NOT NULL DEFAULT now()
);
CREATE INDEX events_undispatched_idx ON events (created_at) WHERE dispatched_at IS NULL;

-- The "build any workflow" definition. Created disabled; explicit enable (§9.5).
CREATE TABLE workflows (
    id                       UUID  PRIMARY KEY,
    name                     TEXT  NOT NULL,
    description              TEXT,
    owner_id                 UUID  NOT NULL REFERENCES users(id),   -- run principal (§7c)
    project_id               UUID  REFERENCES projects(id),         -- scope (NULL = owner-global, discouraged)
    enabled                  BOOL  NOT NULL DEFAULT false,
    trigger_event_type       TEXT  NOT NULL,
    trigger_scope            JSONB NOT NULL DEFAULT '{}'::jsonb,    -- e.g. {project_id, kb_id, mime in [...]}
    trigger_on_system_events BOOL  NOT NULL DEFAULT false,          -- react to agent/workflow events (advanced; guarded)
    condition                JSONB,                                 -- SAFE structured filter (§6); NO code eval
    coalesce_window_secs     INT   NOT NULL DEFAULT 0,              -- batch debounce (§7b; honoured in a later slice)
    action_type              TEXT  NOT NULL CHECK (action_type IN ('agent_run', 'system_action')),
    agent_id                 UUID  REFERENCES agents(id),
    action_config            JSONB NOT NULL DEFAULT '{}'::jsonb,    -- prompt template / system-action params
    max_runs_per_window      INT   NOT NULL DEFAULT 60,             -- rate cap (§7b; honoured in a later slice)
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    version                  INT   NOT NULL DEFAULT 1
);
-- Match path: enabled workflows by trigger type (the dispatcher's hot lookup).
CREATE INDEX workflows_match_idx ON workflows (trigger_event_type) WHERE enabled;

-- Each firing of a workflow. Mirrors the agent-run shape (depth, run-as, outcome).
CREATE TABLE workflow_runs (
    id                UUID                PRIMARY KEY,
    workflow_id       UUID                NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    trigger_event_ids UUID[]              NOT NULL DEFAULT '{}',
    status            workflow_run_status NOT NULL DEFAULT 'queued',
    depth             INT                 NOT NULL DEFAULT 0,
    run_as_user_id    UUID                REFERENCES users(id),
    outcome           JSONB,
    error             TEXT,
    started_at        TIMESTAMPTZ,
    finished_at       TIMESTAMPTZ,
    created_at        TIMESTAMPTZ         NOT NULL DEFAULT now()
);
CREATE INDEX workflow_runs_workflow_idx ON workflow_runs (workflow_id);
-- Idempotency (§7a.6, §12.8): a replayed (workflow, event-set) cannot double-run.
CREATE UNIQUE INDEX workflow_runs_idem_idx ON workflow_runs (workflow_id, trigger_event_ids);
