-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
ALTER TABLE routed_jobs DROP CONSTRAINT routed_jobs_kind_check;
ALTER TABLE routed_jobs ADD CONSTRAINT routed_jobs_kind_check
    CHECK (kind IN ('verify_draft','deep_research'));
COMMENT ON COLUMN routed_jobs.result_id IS
    'Verification run for verify_draft; final assistant message for deep_research';
