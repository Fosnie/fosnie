-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
ALTER TABLE routed_jobs DROP CONSTRAINT routed_jobs_status_check;
ALTER TABLE routed_jobs ADD CONSTRAINT routed_jobs_status_check
    CHECK(status IN ('queued','running','awaiting_input','succeeded','failed','cancelled','interrupted'));
ALTER TABLE routed_jobs ADD CONSTRAINT routed_jobs_clarification_kind
    CHECK(status <> 'awaiting_input' OR kind = 'deep_research');
CREATE INDEX routed_jobs_waiting_deadline ON routed_jobs(deadline) WHERE status='awaiting_input';
