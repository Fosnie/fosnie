-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE workflow_routing_inputs (
    run_id UUID PRIMARY KEY REFERENCES workflow_runs(id) ON DELETE CASCADE,
    input JSONB NOT NULL,
    actor JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE routed_jobs DROP CONSTRAINT routed_jobs_kind_check;
ALTER TABLE routed_jobs ADD CONSTRAINT routed_jobs_kind_check
    CHECK(kind IN ('verify_draft','deep_research','workflow_agent'));
-- Called at reservation and publication fences, also while a stream is idle.
CREATE FUNCTION routing_workflow_active(job UUID) RETURNS BOOLEAN LANGUAGE sql STABLE AS $$
    SELECT EXISTS(SELECT 1 FROM workflow_runs r JOIN workflows w ON w.id=r.workflow_id
        JOIN workflow_routing_inputs i ON i.run_id=r.id JOIN users u ON u.id=r.run_as_user_id
        WHERE r.id=job AND r.status='running' AND w.enabled AND w.owner_id=r.run_as_user_id
        AND u.deactivated_at IS NULL AND u.role::text=i.actor->>'role'
        AND EXISTS(SELECT 1 FROM config_settings WHERE key='routing.workflow_jobs_enabled' AND value_type='bool' AND value='true')
        AND NOT EXISTS(SELECT 1 FROM config_settings WHERE key='workflows.pause_all' AND value='true'));
$$;
