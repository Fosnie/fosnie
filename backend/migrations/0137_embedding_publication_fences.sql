-- Bind the built corpus to model-free publication; keep legacy intents readable.
ALTER TABLE embedding_rebuild_attempts ADD COLUMN source_sha256 TEXT
    CHECK(source_sha256 IS NULL OR source_sha256 ~ '^[0-9a-f]{64}$');
ALTER TABLE embedding_promotions ADD COLUMN source_sha256 TEXT
    CHECK(source_sha256 IS NULL OR source_sha256 ~ '^[0-9a-f]{64}$');
ALTER TABLE embedding_promotions DROP CONSTRAINT embedding_promotions_status_check;
ALTER TABLE embedding_promotions ADD CONSTRAINT embedding_promotions_status_check
    CHECK(status IN ('pending','completed','failed'));
ALTER TABLE embedding_promotions ADD COLUMN routed_job_id UUID REFERENCES routed_jobs(id);
-- Existing routed pending intents may only confirm an already-published alias;
-- without a source receipt, a not-yet-published attempt is refused safely.
UPDATE embedding_promotions p SET routed_job_id=(
    SELECT a.routed_job_id FROM embedding_rebuild_attempts a
    WHERE a.status='built' AND a.routed_job_id IS NOT NULL
      AND a.desired_revision=p.desired_revision AND a.new_collection=p.new_collection
    ORDER BY a.created_at DESC LIMIT 1
) WHERE p.status='pending';
