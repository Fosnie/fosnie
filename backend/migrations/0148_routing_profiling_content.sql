-- Original primary responses and the two frozen judge slots share the job's
-- allocation, deadline, ownership and cancellation fence. No replay worker.
ALTER TABLE routing_behaviour_attempts ADD COLUMN content_primary JSONB;
CREATE TABLE routing_behaviour_content_groups (
 job_id UUID NOT NULL REFERENCES routing_behaviour_jobs(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL, case_id TEXT NOT NULL, contract JSONB, grade JSONB,
 PRIMARY KEY(job_id,ordinal)
);
CREATE TABLE routing_behaviour_content_calls (
 job_id UUID NOT NULL, group_ordinal INTEGER NOT NULL,
 order_no INTEGER NOT NULL CHECK(order_no IN (0,1)),
 status TEXT NOT NULL DEFAULT 'prepared' CHECK(status IN ('prepared','sent','completed','unknown','skipped')),
 binding JSONB, original_row JSONB, sent_at TIMESTAMPTZ, finished_at TIMESTAMPTZ,
 PRIMARY KEY(job_id,group_ordinal,order_no),
 FOREIGN KEY(job_id,group_ordinal) REFERENCES routing_behaviour_content_groups(job_id,ordinal) ON DELETE CASCADE
);
CREATE FUNCTION routing_behaviour_content_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF (OLD.contract IS NOT NULL AND NEW.contract IS DISTINCT FROM OLD.contract)
 OR (OLD.grade IS NOT NULL AND NEW.grade IS DISTINCT FROM OLD.grade)
 OR ROW(NEW.job_id,NEW.ordinal,NEW.case_id) IS DISTINCT FROM ROW(OLD.job_id,OLD.ordinal,OLD.case_id) THEN
  RAISE EXCEPTION 'profiling content contract and grade are write-once' USING ERRCODE='23514';
 END IF;
 RETURN NEW; END $$;
CREATE TRIGGER routing_behaviour_content_once BEFORE UPDATE ON routing_behaviour_content_groups
 FOR EACH ROW EXECUTE FUNCTION routing_behaviour_content_once();
CREATE FUNCTION routing_behaviour_primary_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF OLD.content_primary IS NOT NULL AND NEW.content_primary IS DISTINCT FROM OLD.content_primary THEN
  RAISE EXCEPTION 'profiling original response is write-once' USING ERRCODE='23514';
 END IF;
 RETURN NEW; END $$;
CREATE TRIGGER routing_behaviour_primary_once BEFORE UPDATE ON routing_behaviour_attempts
 FOR EACH ROW EXECUTE FUNCTION routing_behaviour_primary_once();
CREATE FUNCTION routing_behaviour_judge_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 IF ROW(NEW.job_id,NEW.group_ordinal,NEW.order_no) IS DISTINCT FROM ROW(OLD.job_id,OLD.group_ordinal,OLD.order_no)
 OR (OLD.binding IS NOT NULL AND NEW.binding IS DISTINCT FROM OLD.binding)
 OR (OLD.original_row IS NOT NULL AND NEW.original_row IS DISTINCT FROM OLD.original_row)
 OR NOT ((OLD.status='prepared' AND NEW.status IN ('sent','skipped')) OR (OLD.status='sent' AND NEW.status IN ('completed','unknown'))) THEN
  RAISE EXCEPTION 'profiling judge slot cannot be replayed or replaced' USING ERRCODE='23514';
 END IF;
 RETURN NEW; END $$;
CREATE TRIGGER routing_behaviour_judge_once BEFORE UPDATE ON routing_behaviour_content_calls
 FOR EACH ROW EXECUTE FUNCTION routing_behaviour_judge_once();
