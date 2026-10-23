-- CPU-only selector replay has its own durable bounded job, never an inference budget.
CREATE TABLE routing_query_jobs (
 id UUID PRIMARY KEY,actor_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 policy_id UUID NOT NULL REFERENCES routing_policies(id) ON DELETE CASCADE,
 plan JSONB NOT NULL,deadline TIMESTAMPTZ NOT NULL,
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','completed','cancelled','failed','interrupted')),
 worker UUID,assessment_sha256 TEXT,error_code TEXT,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX routing_query_one_active_actor ON routing_query_jobs(actor_user_id) WHERE status IN ('queued','running');
CREATE TABLE routing_query_assessments (
 sha256 TEXT PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 actor_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 policy_id UUID NOT NULL REFERENCES routing_policies(id) ON DELETE CASCADE,
 job_id UUID NOT NULL UNIQUE REFERENCES routing_query_jobs(id),
 cohort_sha256 TEXT NOT NULL REFERENCES routing_calibration_cohorts(sha256),
 assessment JSONB NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE routing_query_validation_claims (
 cohort_sha256 TEXT NOT NULL REFERENCES routing_calibration_cohorts(sha256),
 actor_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 lineage_sha256 TEXT NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(cohort_sha256,actor_user_id)
);
CREATE INDEX routing_query_assessment_lookup ON routing_query_assessments(actor_user_id,policy_id,created_at DESC);
CREATE TRIGGER routing_query_assessment_immutable BEFORE UPDATE ON routing_query_assessments FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();
CREATE TRIGGER routing_query_validation_immutable BEFORE UPDATE ON routing_query_validation_claims FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();
CREATE FUNCTION routing_query_plan_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF ROW(NEW.actor_user_id,NEW.policy_id,NEW.plan,NEW.deadline) IS DISTINCT FROM ROW(OLD.actor_user_id,OLD.policy_id,OLD.plan,OLD.deadline) THEN
  RAISE EXCEPTION 'query calibration plan is immutable' USING ERRCODE='23514';
 END IF; RETURN NEW; END $$;
CREATE TRIGGER routing_query_plan_immutable BEFORE UPDATE ON routing_query_jobs FOR EACH ROW EXECUTE FUNCTION routing_query_plan_immutable();
