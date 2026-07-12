-- Copyright 2026 Private AI Ltd (SC881079)
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.
-- FEATURE A2 — enforce append-only at the DB layer (acceptance criterion #3).
--
-- The skeleton (0001) documented "INSERT/SELECT only" as a deployment grant. We
-- make it real with a row-level trigger that rejects UPDATE/DELETE on the two
-- tamper-evident tables — so the chain is not forgeable even by a role that holds
-- UPDATE/DELETE (e.g. the DB owner). The hash chain + signed checkpoints already
-- make tampering *detectable*; this makes the ordinary path *impossible*.
--
-- Scope: audit_events (+ its partitions, via the partitioned-parent trigger) and
-- audit_checkpoints. NOT interaction_evidence / subject_keys — those need the
-- retention sweep (delete old rows) and crypto-shred (delete keys) to write.
--
-- Retention's partition rotation uses DROP TABLE (DDL), which row triggers never
-- fire on, so dropping whole partitions still works. A privileged operator who
-- genuinely must rewrite history can `SET session_replication_role = replica` to
-- bypass the trigger — that is exactly the DB-level attacker the chain + the
-- signed checkpoints are designed to expose after the fact.

CREATE OR REPLACE FUNCTION reject_audit_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'audit log is append-only: % on % is not permitted', TG_OP, TG_TABLE_NAME
        USING ERRCODE = 'insufficient_privilege';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION reject_audit_mutation();

CREATE TRIGGER audit_checkpoints_append_only
    BEFORE UPDATE OR DELETE ON audit_checkpoints
    FOR EACH ROW EXECUTE FUNCTION reject_audit_mutation();
