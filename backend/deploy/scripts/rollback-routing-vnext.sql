-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- MAINTENANCE ONLY: stop backend/workers, drain leases, and take a database
-- backup first. This is a source artifact, never an automatic startup action.
BEGIN;
LOCK TABLE provider_configs IN ACCESS EXCLUSIVE MODE;
CREATE TABLE IF NOT EXISTS routing_vnext_metadata_archive (
    archive_id UUID NOT NULL,kind TEXT NOT NULL,entity_id UUID NOT NULL,payload JSONB NOT NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT now(),PRIMARY KEY(archive_id,kind,entity_id)
);
CREATE TEMP TABLE routing_vnext_rollback_id AS SELECT gen_random_uuid() AS id;
-- Keep versioned chat bindings fail-closed for older clients. Do not silently
-- replace unavailable profiles/variants with a default during maintenance.
INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload)
SELECT a.id,'chat_binding',c.id,jsonb_build_object('routing_intent',c.model_selection,'options',c.routing_options)
FROM chats c CROSS JOIN routing_vnext_rollback_id a WHERE c.model_selection->>'schema_version'='2';
INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload)
SELECT a.id,'variant',v.id,v.variant FROM model_variants v CROSS JOIN routing_vnext_rollback_id a;
INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload)
SELECT a.id,'model',p.id,jsonb_build_object('scope',p.scope,'scope_id',p.scope_id,
    'connection_id',p.connection_id,'metadata',p.model_metadata,'tariffs',p.model_pricing,
    'model_config_revision',p.model_config_revision)
FROM provider_configs p CROSS JOIN routing_vnext_rollback_id a WHERE p.role='llm';
INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload)
SELECT a.id,'connection',p.id,jsonb_build_object('scope',p.scope,'scope_id',p.scope_id,'label',p.label,
    'adapter',p.connection_adapter,'quota_group_id',p.quota_group_id,'metadata',p.model_metadata)
FROM provider_configs p CROSS JOIN routing_vnext_rollback_id a WHERE p.role='connection';
-- The archive deliberately has no plaintext key, ciphertext key, endpoint or
-- secret fingerprint. Live credentials are restored into the original store.
DO $$ BEGIN
 IF to_regclass(current_schema()||'.routing_health_bindings') IS NOT NULL THEN
  DROP TABLE routing_health_bindings; -- private in-flight provenance, never archived
  DROP FUNCTION routing_health_binding_immutable();
 END IF;
 IF EXISTS(SELECT 1 FROM information_schema.columns WHERE table_name='routing_policies' AND column_name='profile_config' AND table_schema=current_schema()) THEN
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''routing_profile'',p.id,jsonb_build_object(''label'',p.label,''scope'',p.scope,''scope_id'',p.scope_id,''policy'',p.policy,''config'',p.profile_config) FROM routing_policies p CROSS JOIN routing_vnext_rollback_id a WHERE p.profile_config IS NOT NULL';
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''project_binding'',p.id,jsonb_build_object(''routing_intent'',p.routing_intent,''revision'',p.routing_revision) FROM projects p CROSS JOIN routing_vnext_rollback_id a WHERE p.routing_intent IS NOT NULL';
  ALTER TABLE projects DROP COLUMN routing_intent,DROP COLUMN routing_revision;
  ALTER TABLE routing_policies DROP COLUMN profile_config;
 END IF;
END $$;
DO $$ BEGIN
 IF to_regclass(current_schema()||'.routing_health_scopes') IS NOT NULL THEN
  DROP TABLE routing_health_scopes; -- ephemeral private health; no binding hashes in archive
 END IF;
 -- An older binary cannot decode the new model-specific failure kinds.
 DELETE FROM routing_provider_health WHERE failure->>'kind' IN ('access_denied','model_unavailable');
 IF to_regclass(current_schema()||'.routing_context_projections') IS NOT NULL THEN
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''context_projection'',substr(p.sha256,1,32)::uuid,jsonb_build_object(''bundle'',p.bundle,''receipt'',p.receipt,''sha256'',p.sha256,''source_artifact_sha256'',p.source_artifact_sha256) FROM routing_context_projections p CROSS JOIN routing_vnext_rollback_id a';
  DROP TABLE routing_context_projections;
 END IF;
 IF to_regclass(current_schema()||'.routing_query_assessments') IS NOT NULL THEN
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''query_assessment'',q.job_id,q.assessment FROM routing_query_assessments q CROSS JOIN routing_vnext_rollback_id a';
  DROP TABLE routing_query_assessments,routing_query_validation_claims,routing_query_jobs;
  DROP FUNCTION routing_query_plan_immutable();
 END IF;
 IF to_regclass(current_schema()||'.routing_calibration_cohorts') IS NOT NULL THEN
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''calibration_cohort'',b.job_id,jsonb_build_object(''cohort'',b.cohort,''sha256'',b.sha256) FROM routing_calibration_cohorts b CROSS JOIN routing_vnext_rollback_id a';
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''quality_assessment'',substr(b.sha256,1,32)::uuid,jsonb_build_object(''assessment'',b.assessment,''sha256'',b.sha256) FROM routing_quality_assessments b CROSS JOIN routing_vnext_rollback_id a';
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''economics_trace'',m.id,m.routing_economics FROM messages m CROSS JOIN routing_vnext_rollback_id a WHERE m.routing_economics IS NOT NULL';
  ALTER TABLE messages DROP COLUMN routing_economics;
  DROP TABLE routing_quality_assessments,routing_calibration_cohorts;
 END IF;
 IF to_regclass(current_schema()||'.routing_behaviour_artifacts') IS NOT NULL THEN
  EXECUTE 'INSERT INTO routing_vnext_metadata_archive(archive_id,kind,entity_id,payload) SELECT a.id,''behaviour_profile'',b.job_id,jsonb_build_object(''scope'',b.scope,''owner_user_id'',b.owner_user_id,''bundle'',b.bundle,''sha256'',b.sha256) FROM routing_behaviour_artifacts b CROSS JOIN routing_vnext_rollback_id a';
  DROP TABLE routing_behaviour_bindings,routing_behaviour_artifacts,routing_behaviour_attempts,routing_behaviour_jobs;
  DROP FUNCTION routing_behaviour_immutable();
  DROP FUNCTION routing_behaviour_allocation_immutable();
 END IF;
END $$;
SELECT materialise_legacy_provider_connections();
DROP TRIGGER provider_connection ON provider_configs;
DROP TRIGGER provider_connection_propagate ON provider_configs;
DROP TRIGGER provider_connection_cleanup ON provider_configs;
DROP TRIGGER provider_z_catalogue_changed ON provider_configs;
DROP TRIGGER profile_catalogue_changed ON model_profiles;
DROP TABLE model_variants;
DROP FUNCTION provider_connection_link();
DROP FUNCTION provider_connection_propagate();
DROP FUNCTION provider_connection_cleanup();
DROP FUNCTION routing_catalogue_changed();
DROP FUNCTION materialise_legacy_provider_connections();
DROP TABLE routing_catalogue_state;
DROP TABLE routing_catalogue_generations;
ALTER TABLE provider_configs DROP CONSTRAINT provider_connection_shape;
ALTER TABLE provider_configs DROP CONSTRAINT provider_connection_role;
ALTER TABLE provider_configs DROP COLUMN connection_id,DROP COLUMN auth_revision,
    DROP COLUMN model_config_revision,DROP COLUMN connection_auth_revision,DROP COLUMN connection_adapter,
    DROP COLUMN quota_group_id,DROP COLUMN legacy_owner_provider_id,DROP COLUMN model_metadata,
    DROP COLUMN model_pricing,DROP COLUMN catalogue_entry_revision;
CREATE OR REPLACE FUNCTION bump_provider_routing_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.base_url,NEW.model,NEW.api_key_encrypted,NEW.enabled,NEW.reasoning_mode)
        IS DISTINCT FROM ROW(OLD.base_url,OLD.model,OLD.api_key_encrypted,OLD.enabled,OLD.reasoning_mode) THEN
        NEW.routing_revision:=OLD.routing_revision+1;
    ELSE NEW.routing_revision:=OLD.routing_revision; END IF;
    RETURN NEW;
END;
$$;
DROP INDEX provider_configs_deployment_uniq;
DROP INDEX provider_configs_user_uniq;
CREATE UNIQUE INDEX provider_configs_deployment_uniq ON provider_configs(role) WHERE scope='deployment' AND role<>'llm';
CREATE UNIQUE INDEX provider_configs_user_uniq ON provider_configs(role,scope_id) WHERE scope='user' AND role<>'llm';
-- Otherwise an older SQLx binary rejects an unrecognised applied migration.
DELETE FROM _sqlx_migrations WHERE version IN (139,140,141,142,143,144,145,146,147);
DROP TABLE routing_vnext_rollback_id;
COMMIT;
-- Metadata remains in the archive for authorised, validated re-import after
-- re-upgrade. It is not active evidence. Refresh legacy profiles explicitly;
-- old bindings and evaluation hashes are never rewritten into a passing state.
