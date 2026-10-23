-- Immutable public behavioural evidence and a single bounded allocation/attempt ledger.
CREATE TABLE routing_behaviour_jobs (
 id UUID PRIMARY KEY,scope TEXT NOT NULL CHECK(scope IN ('deployment','user')),
 owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE,actor JSONB NOT NULL,
 revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation'),
 plan JSONB NOT NULL,allocation JSONB NOT NULL,usage JSONB NOT NULL DEFAULT '{}',
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','completed','failed','cancelled','interrupted')),
 worker UUID,lease_until TIMESTAMPTZ,deadline TIMESTAMPTZ NOT NULL,
 artifact_sha256 TEXT,error_code TEXT,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK((scope='user' AND owner_user_id IS NOT NULL) OR (scope='deployment' AND owner_user_id IS NULL))
);
CREATE TABLE routing_behaviour_attempts (
 job_id UUID NOT NULL REFERENCES routing_behaviour_jobs(id) ON DELETE CASCADE,ordinal INTEGER NOT NULL,
 variant_id UUID NOT NULL,variant_revision BIGINT NOT NULL,behaviour_sha256 TEXT NOT NULL,case_id TEXT NOT NULL,
 status TEXT NOT NULL DEFAULT 'prepared' CHECK(status IN ('prepared','sent','completed','failed','unknown')),
 binding JSONB,observation JSONB,receipt_sha256 TEXT,sent_at TIMESTAMPTZ,finished_at TIMESTAMPTZ,
 PRIMARY KEY(job_id,ordinal),UNIQUE(job_id,variant_id,case_id)
);
CREATE TABLE routing_behaviour_artifacts (
 sha256 TEXT PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),scope TEXT NOT NULL CHECK(scope IN ('deployment','user')),
 owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE,job_id UUID NOT NULL UNIQUE REFERENCES routing_behaviour_jobs(id),
 bundle JSONB NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK((scope='user' AND owner_user_id IS NOT NULL) OR (scope='deployment' AND owner_user_id IS NULL))
);
CREATE TABLE routing_behaviour_bindings (
 receipt_sha256 TEXT PRIMARY KEY,artifact_sha256 TEXT NOT NULL REFERENCES routing_behaviour_artifacts(sha256),
 actor_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,pool_hash TEXT NOT NULL,snapshot JSONB NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('profiled','limited')),created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(artifact_sha256,actor_user_id,pool_hash)
);
CREATE INDEX routing_behaviour_jobs_owner ON routing_behaviour_jobs(owner_user_id,created_at DESC);
-- Application writes are append-only. No UPDATE/rebind may relabel prior evidence.
CREATE FUNCTION routing_behaviour_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 RAISE EXCEPTION 'routing behavioural artifacts are immutable' USING ERRCODE='23514'; END $$;
CREATE TRIGGER routing_behaviour_artifact_immutable BEFORE UPDATE ON routing_behaviour_artifacts FOR EACH ROW EXECUTE FUNCTION routing_behaviour_immutable();
CREATE FUNCTION routing_behaviour_allocation_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF ROW(NEW.scope,NEW.owner_user_id,NEW.actor,NEW.plan,NEW.allocation,NEW.deadline)
 IS DISTINCT FROM ROW(OLD.scope,OLD.owner_user_id,OLD.actor,OLD.plan,OLD.allocation,OLD.deadline) THEN
  RAISE EXCEPTION 'profiling preview allocation is immutable' USING ERRCODE='23514';
 END IF;
 RETURN NEW; END $$;
CREATE TRIGGER routing_behaviour_allocation_immutable BEFORE UPDATE ON routing_behaviour_jobs FOR EACH ROW EXECUTE FUNCTION routing_behaviour_allocation_immutable();
