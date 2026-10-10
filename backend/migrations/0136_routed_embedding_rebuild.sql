ALTER TABLE routed_jobs DROP CONSTRAINT routed_jobs_kind_check;
ALTER TABLE routed_jobs ADD CONSTRAINT routed_jobs_kind_check CHECK(kind IN ('verify_draft','deep_research','workflow_agent','ingest','reindex'));
ALTER TABLE embedding_rebuild_attempts ADD COLUMN routed_job_id UUID REFERENCES routed_jobs(id);
CREATE TABLE embedding_retired_indices (
    collection_name TEXT PRIMARY KEY,
    promotion_id UUID NOT NULL REFERENCES embedding_promotions(id),
    retired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL DEFAULT 'retained' CHECK(status IN ('retained','deleted')),
    deleted_at TIMESTAMPTZ
);
