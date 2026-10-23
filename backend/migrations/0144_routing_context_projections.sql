-- CPU projections retain original native evidence, scope and expiry.
CREATE TABLE routing_context_projections (
 sha256 TEXT PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 scope TEXT NOT NULL CHECK(scope IN ('deployment','user')),
 owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE,
 source_artifact_sha256 TEXT NOT NULL REFERENCES routing_behaviour_artifacts(sha256),
 bundle JSONB NOT NULL,receipt JSONB NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK((scope='user' AND owner_user_id IS NOT NULL) OR (scope='deployment' AND owner_user_id IS NULL))
);
CREATE INDEX routing_context_projections_owner ON routing_context_projections(owner_user_id,created_at DESC);
CREATE TRIGGER routing_context_projection_immutable BEFORE UPDATE ON routing_context_projections
 FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();
