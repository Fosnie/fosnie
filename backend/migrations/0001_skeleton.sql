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

-- 0001_skeleton.sql — cross-cutting tables for the PAI Platform backend skeleton.
--
-- Scope: config (runtime-mutable) + branding, the audit hash-chain, and the
-- durable background-task queue. Identity and feature tables (users, groups,
-- access_grants, projects, chats, …) are laid in by their own slices; the
-- nullable `*_by` / `actor_user_id` columns here gain FKs to `users` then.
--
-- Conventions: single-tenant (no
-- tenant_id), UUIDv7 ids minted application-side, timestamptz in UTC.
-- Forward-only; owned by sqlx-cli.

-- ---------------------------------------------------------------------------
-- Enums
-- ---------------------------------------------------------------------------

-- Typed config values (typed validated records, not a blob).
CREATE TYPE config_value_type AS ENUM ('string', 'int', 'float', 'bool', 'json');

-- Branding asset kinds.
CREATE TYPE branding_kind AS ENUM ('logo', 'favicon');

-- Audit outcome.
CREATE TYPE audit_outcome AS ENUM ('success', 'failure');

-- Durable task queue.
CREATE TYPE task_type AS ENUM ('ingest', 'automation_run', 'audit_retention', 'artefact_cleanup');
CREATE TYPE task_status AS ENUM ('queued', 'running', 'succeeded', 'failed', 'dead_letter');

-- ---------------------------------------------------------------------------
-- config_settings — runtime-mutable typed config
-- ---------------------------------------------------------------------------
CREATE TABLE config_settings (
    key            TEXT PRIMARY KEY,
    value          TEXT              NOT NULL,
    value_type     config_value_type NOT NULL,
    scope          TEXT              NOT NULL DEFAULT 'global',
    validation_ref TEXT,
    updated_by     UUID,                      -- FK users(id) added in the auth slice
    updated_at     TIMESTAMPTZ       NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- branding_assets — logo/favicon pointers (files on disk)
-- ---------------------------------------------------------------------------
CREATE TABLE branding_assets (
    id         UUID          PRIMARY KEY,
    kind       branding_kind NOT NULL,
    disk_path  TEXT          NOT NULL,
    mime       TEXT          NOT NULL,
    updated_by UUID,                          -- FK users(id) added in the auth slice
    updated_at TIMESTAMPTZ   NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- audit_events — append-only hash-chain
--
-- Range-partitioned monthly on occurred_at so 24-month retention drops whole
-- partitions rather than deleting rows (§9.2). `seq` is a global monotonic
-- chain index taken from a sequence under an advisory lock at append time;
-- each row's `hash` = SHA256(canonical(row) || prev_hash), binding it to its
-- predecessor. Verification recomputes the chain in `seq` order.
--
-- Separation of duties (§A.2.2): the application role is granted INSERT/SELECT
-- only — no UPDATE/DELETE — so the log is not forgeable through the app even by
-- an admin. Retention deletes by dropping partitions (privileged path).
-- The grant is applied at deployment against the real role; documented here.
-- ---------------------------------------------------------------------------
CREATE SEQUENCE audit_events_seq AS BIGINT START 1;

CREATE TABLE audit_events (
    seq                       BIGINT        NOT NULL,
    id                        UUID          NOT NULL,
    actor_user_id             UUID,
    actor_role                TEXT          NOT NULL,
    action_type               TEXT          NOT NULL,
    resource_type             TEXT,
    resource_id               UUID,
    occurred_at               TIMESTAMPTZ   NOT NULL,
    session_id                TEXT,
    source_ip                 TEXT,
    outcome                   audit_outcome NOT NULL,
    outcome_reason            TEXT,
    model_agent_traceability  JSONB,
    token_usage               JSONB,
    risk_anomaly_flag         BOOLEAN       NOT NULL DEFAULT false,
    payload                   JSONB,
    prev_hash                 BYTEA,                 -- NULL only for the genesis row
    hash                      BYTEA         NOT NULL,
    signature                 BYTEA,                 -- optional Ed25519 (deferred)
    PRIMARY KEY (occurred_at, seq)                   -- partition key must be in the PK
) PARTITION BY RANGE (occurred_at);

-- Helps chain verification read in order within the retention window.
CREATE INDEX audit_events_seq_idx ON audit_events (seq);

-- Initial partitions: previous, current, next month, plus a catch-all default
-- so inserts never fail before the retention job rolls partitions forward.
CREATE TABLE audit_events_2026_04 PARTITION OF audit_events
    FOR VALUES FROM ('2026-04-01 00:00:00+00') TO ('2026-05-01 00:00:00+00');
CREATE TABLE audit_events_2026_05 PARTITION OF audit_events
    FOR VALUES FROM ('2026-05-01 00:00:00+00') TO ('2026-06-01 00:00:00+00');
CREATE TABLE audit_events_2026_06 PARTITION OF audit_events
    FOR VALUES FROM ('2026-06-01 00:00:00+00') TO ('2026-07-01 00:00:00+00');
CREATE TABLE audit_events_default PARTITION OF audit_events DEFAULT;

-- ---------------------------------------------------------------------------
-- tasks — durable background-task queue (schema §15.1, topology §7.1)
-- ---------------------------------------------------------------------------
CREATE TABLE tasks (
    id              UUID        PRIMARY KEY,
    task_type       task_type   NOT NULL,
    payload         JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status          task_status NOT NULL DEFAULT 'queued',
    priority        INT         NOT NULL DEFAULT 100,   -- lower runs first; live work outranks
    retry_count     INT         NOT NULL DEFAULT 0,
    max_retries     INT         NOT NULL DEFAULT 5,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    automation_id   UUID                                 -- FK automations(id) added later
);

-- Claim query: WHERE status='queued' AND next_attempt_at<=now()
--              ORDER BY priority, next_attempt_at  FOR UPDATE SKIP LOCKED
CREATE INDEX tasks_claim_idx ON tasks (status, next_attempt_at, priority);
