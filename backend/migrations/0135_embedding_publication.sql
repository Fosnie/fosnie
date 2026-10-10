-- Model-free embedding publication recovery; credentials remain in provenance.
CREATE TABLE embedding_promotions (
    id UUID PRIMARY KEY,
    old_collection TEXT NOT NULL,
    new_collection TEXT NOT NULL,
    desired_revision BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','completed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK(old_collection<>new_collection)
);
CREATE UNIQUE INDEX embedding_promotion_pending ON embedding_promotions((1)) WHERE status='pending';
CREATE TABLE embedding_rebuild_attempts (
    task_id UUID PRIMARY KEY REFERENCES tasks(id),
    desired_revision BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'building' CHECK(status IN ('building','built','completed','failed')),
    new_collection TEXT,
    old_collection TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
