-- A confirmed verification may continue once, without renewing its ledger.
ALTER TABLE routed_jobs ADD COLUMN phase TEXT NOT NULL DEFAULT 'verify'
    CHECK (phase IN ('verify', 'repair'));
ALTER TABLE routed_jobs ADD COLUMN verified_checkpoint JSONB;
COMMENT ON COLUMN routed_jobs.verified_checkpoint IS
    'Private validated draft and source spans; never exposed in task payloads or status APIs';
