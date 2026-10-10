-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Server-owned checkpoints; task payloads contain only this row's ID.
CREATE TABLE routed_jobs (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL UNIQUE REFERENCES tasks(id),
    actor_user_id UUID NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK(kind='verify_draft'),
    context JSONB NOT NULL,
    plan JSONB NOT NULL,
    limits JSONB NOT NULL,
    usage JSONB NOT NULL,
    input JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','succeeded','failed','cancelled','interrupted')),
    owner UUID,
    lease_until TIMESTAMPTZ,
    deadline TIMESTAMPTZ NOT NULL,
    result_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((status='running') = (owner IS NOT NULL AND lease_until IS NOT NULL))
);
CREATE INDEX routed_jobs_recovery ON routed_jobs(status,lease_until) WHERE status IN ('queued','running');
