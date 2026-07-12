-- Durable agent runs (action-taking agents).
--
-- A run wraps a chat turn (or an unattended automation run) with the controller loop;
-- it can PAUSE for human approval on a gated (state-changing / egress) tool and RESUME
-- later, surviving a process crash via `checkpoint` (the message list + step index).
-- `pending_tool`/`pending_args`/`pending_step` hold the exact call the human approves —
-- resume executes it verbatim, never re-inferring. The hash-chain audit, keyed by this
-- run's id, is the trajectory log.

CREATE TYPE agent_run_status AS ENUM (
    'running', 'awaiting_approval', 'approved', 'rejected',
    'completed', 'failed', 'cancelled'
);

CREATE TABLE agent_runs (
    id              UUID             PRIMARY KEY,
    agent_id        UUID             REFERENCES agents(id),
    acting_user_id  UUID             REFERENCES users(id),
    chat_id         UUID,
    turn_id         UUID,
    project_id      UUID,
    automation_id   UUID             REFERENCES automations(id) ON DELETE SET NULL,
    status          agent_run_status NOT NULL DEFAULT 'running',
    step_count      INT              NOT NULL DEFAULT 0,
    token_used      INT              NOT NULL DEFAULT 0,
    -- The pending gated call awaiting approval (executed verbatim on resume).
    pending_tool    TEXT,
    pending_args    JSONB,
    pending_step    INT,
    -- Crash-resume state: the message list + step at the pause point.
    checkpoint      JSONB,
    created_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ      NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ
);

CREATE INDEX agent_runs_user_idx ON agent_runs (acting_user_id, created_at DESC);
CREATE INDEX agent_runs_awaiting_idx ON agent_runs (status) WHERE status = 'awaiting_approval';

-- Durable resume path for an approved (or owner-approved unattended) run.
ALTER TYPE task_type ADD VALUE IF NOT EXISTS 'agent_resume';
