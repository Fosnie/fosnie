-- Quality admission is derived only from durable, bounded public attempts.
CREATE TABLE routing_calibration_cohorts (
 sha256 TEXT PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 scope TEXT NOT NULL CHECK(scope IN ('deployment','user')),
 owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE,
 job_id UUID NOT NULL UNIQUE REFERENCES routing_behaviour_jobs(id),
 cohort JSONB NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK((scope='user' AND owner_user_id IS NOT NULL) OR (scope='deployment' AND owner_user_id IS NULL))
);
CREATE TABLE routing_quality_assessments (
 sha256 TEXT PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 actor_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 policy_id UUID NOT NULL REFERENCES routing_policies(id) ON DELETE CASCADE,
 cohort_sha256 TEXT NOT NULL REFERENCES routing_calibration_cohorts(sha256),
 assessment JSONB NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX routing_quality_assessment_lookup ON routing_quality_assessments(actor_user_id,policy_id,created_at DESC);
CREATE TRIGGER routing_calibration_cohort_immutable BEFORE UPDATE ON routing_calibration_cohorts FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();
CREATE TRIGGER routing_quality_assessment_immutable BEFORE UPDATE ON routing_quality_assessments FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();

ALTER TABLE messages ADD COLUMN routing_economics JSONB;
