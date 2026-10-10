-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- A committed tombstone precedes model planning. Keys are never reclaimed.
CREATE TABLE research_launch_receipts (
    actor_user_id UUID NOT NULL REFERENCES users(id),
    request_id UUID NOT NULL,
    actor_role TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK(length(request_hash)=64),
    status TEXT NOT NULL DEFAULT 'planning' CHECK(status IN ('planning','succeeded','failed')),
    job_id UUID REFERENCES routed_jobs(id),
    deadline TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp()+interval '3 minutes'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(actor_user_id,request_id),
    CHECK ((status='succeeded') = (job_id IS NOT NULL))
);
CREATE INDEX routed_research_history ON routed_jobs(actor_user_id,id DESC) WHERE kind='deep_research';
