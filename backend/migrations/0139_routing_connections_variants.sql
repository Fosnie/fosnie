-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- One encrypted provider store; LLM rows retain their deployment identities.
ALTER TABLE provider_configs ADD COLUMN connection_id UUID REFERENCES provider_configs(id) ON DELETE RESTRICT;
ALTER TABLE provider_configs ADD COLUMN auth_revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation');
ALTER TABLE provider_configs ADD COLUMN model_config_revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation');
ALTER TABLE provider_configs ADD COLUMN connection_auth_revision BIGINT;
ALTER TABLE provider_configs ADD COLUMN connection_adapter TEXT CHECK (connection_adapter IN ('openai_chat','openai_responses','anthropic','gemini'));
ALTER TABLE provider_configs ADD COLUMN quota_group_id UUID;
ALTER TABLE provider_configs ADD COLUMN legacy_owner_provider_id UUID;
ALTER TABLE provider_configs ADD COLUMN model_metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(model_metadata)='object');
ALTER TABLE provider_configs ADD COLUMN model_pricing JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(model_pricing)='object');
ALTER TABLE provider_configs ADD COLUMN catalogue_entry_revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation');

DROP INDEX provider_configs_deployment_uniq;
DROP INDEX provider_configs_user_uniq;
CREATE UNIQUE INDEX provider_configs_deployment_uniq ON provider_configs(role) WHERE scope='deployment' AND role NOT IN ('llm','connection');
CREATE UNIQUE INDEX provider_configs_user_uniq ON provider_configs(role,scope_id) WHERE scope='user' AND role NOT IN ('llm','connection');
ALTER TABLE provider_configs ADD CONSTRAINT provider_connection_shape CHECK (role<>'connection' OR
    (connection_id IS NULL AND model IS NULL AND reasoning_mode IS NULL AND NOT is_default
     AND ((scope='deployment' AND scope_id IS NULL) OR (scope='user' AND scope_id IS NOT NULL))));
ALTER TABLE provider_configs ADD CONSTRAINT provider_connection_role CHECK(role='llm' OR connection_id IS NULL);

CREATE TABLE routing_catalogue_state (id BOOLEAN PRIMARY KEY DEFAULT true CHECK(id), active BOOLEAN NOT NULL DEFAULT true);
INSERT INTO routing_catalogue_state(id) VALUES(true);
CREATE TABLE routing_catalogue_generations (scope_key UUID PRIMARY KEY, revision BIGINT NOT NULL CHECK(revision>0));

-- Disable only the old revision trigger during the atomic legacy move. No
-- model/profile/policy UUID, default or dispatch revision is rewritten.
ALTER TABLE provider_configs DISABLE TRIGGER provider_routing_revision;
UPDATE provider_configs SET model_config_revision=routing_revision WHERE role='llm';
INSERT INTO provider_configs(id,role,scope,scope_id,label,base_url,api_key_encrypted,enabled,updated_by,
    legacy_owner_provider_id,connection_adapter,routing_revision,model_metadata)
SELECT gen_random_uuid(),'connection',p.scope,p.scope_id,left(COALESCE(p.label,p.model,'Connection'),120),
    p.base_url,p.api_key_encrypted,true,p.updated_by,p.id,
    CASE WHEN m.profile->>'adapter' IN ('openai_chat','openai_responses','anthropic','gemini') THEN m.profile->>'adapter' END,
    nextval('routing_revision_generation'),jsonb_build_object('locality',COALESCE(m.profile->>'locality','unknown'),'locality_trusted',COALESCE((m.profile->>'locality_trusted')::boolean,false))
FROM provider_configs p LEFT JOIN model_profiles m ON m.provider_id=p.id WHERE p.role='llm';
UPDATE provider_configs p SET connection_id=c.id, connection_auth_revision=c.auth_revision,
    connection_adapter=c.connection_adapter, api_key_encrypted=NULL
FROM provider_configs c WHERE c.role='connection' AND c.legacy_owner_provider_id=p.id AND p.role='llm';
ALTER TABLE provider_configs ENABLE TRIGGER provider_routing_revision;

CREATE FUNCTION provider_connection_link() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE c provider_configs%ROWTYPE;
BEGIN
    IF TG_OP='UPDATE' AND (OLD.role IN ('llm','connection') OR NEW.role IN ('llm','connection'))
        AND ROW(NEW.scope,NEW.scope_id,NEW.role) IS DISTINCT FROM ROW(OLD.scope,OLD.scope_id,OLD.role) THEN
        RAISE EXCEPTION 'provider scope and role are immutable' USING ERRCODE='23514';
    END IF;
    IF NEW.role<>'llm' OR NOT (SELECT active FROM routing_catalogue_state WHERE id) THEN RETURN NEW; END IF;
    IF TG_OP='UPDATE' AND (NEW.scope,NEW.scope_id,NEW.role) IS DISTINCT FROM (OLD.scope,OLD.scope_id,OLD.role) THEN
        RAISE EXCEPTION 'provider scope is immutable' USING ERRCODE='23514';
    END IF;
    IF NEW.connection_id IS NULL THEN
        IF TG_OP='UPDATE' AND OLD.connection_id IS NOT NULL THEN
            RAISE EXCEPTION 'a configured model requires a connection' USING ERRCODE='23514';
        END IF;
        INSERT INTO provider_configs(id,role,scope,scope_id,label,base_url,api_key_encrypted,enabled,
            updated_by,legacy_owner_provider_id,routing_revision)
        VALUES(gen_random_uuid(),'connection',NEW.scope,NEW.scope_id,left(COALESCE(NEW.label,NEW.model,'Connection'),120),
            NEW.base_url,NEW.api_key_encrypted,true,NEW.updated_by,NEW.id,nextval('routing_revision_generation'))
        RETURNING * INTO c;
        NEW.connection_id:=c.id;
    ELSE
        SELECT * INTO c FROM provider_configs WHERE id=NEW.connection_id AND role='connection' FOR UPDATE;
        IF NOT FOUND OR c.scope<>NEW.scope OR c.scope_id IS DISTINCT FROM NEW.scope_id THEN
            RAISE EXCEPTION 'connection scope mismatch' USING ERRCODE='23514';
        END IF;
        IF TG_OP='INSERT' THEN
            IF NEW.api_key_encrypted IS NOT NULL THEN
                RAISE EXCEPTION 'credentials belong to the connection' USING ERRCODE='23514';
            END IF;
        ELSIF NEW.connection_id IS DISTINCT FROM OLD.connection_id THEN
            IF NEW.api_key_encrypted IS NOT NULL THEN
                RAISE EXCEPTION 'credentials belong to the connection' USING ERRCODE='23514';
            END IF;
        ELSIF NEW.api_key_encrypted IS NOT NULL OR NEW.base_url IS DISTINCT FROM c.base_url THEN
            IF c.legacy_owner_provider_id IS DISTINCT FROM NEW.id OR
                EXISTS(SELECT 1 FROM provider_configs WHERE connection_id=c.id AND id<>NEW.id) THEN
                RAISE EXCEPTION 'update shared connection with its expected revision' USING ERRCODE='40001';
            END IF;
            UPDATE provider_configs SET base_url=NEW.base_url,
                api_key_encrypted=COALESCE(NEW.api_key_encrypted,api_key_encrypted),updated_by=NEW.updated_by,updated_at=now()
                WHERE id=c.id RETURNING * INTO c;
        END IF;
    END IF;
    NEW.base_url:=c.base_url;
    NEW.connection_auth_revision:=c.auth_revision;
    NEW.connection_adapter:=c.connection_adapter;
    NEW.api_key_encrypted:=NULL;
    RETURN NEW;
END;
$$;
CREATE TRIGGER provider_connection BEFORE INSERT OR UPDATE ON provider_configs FOR EACH ROW EXECUTE FUNCTION provider_connection_link();

CREATE OR REPLACE FUNCTION bump_provider_routing_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.catalogue_entry_revision:=nextval('routing_revision_generation');
    IF NEW.role='connection' THEN
        IF (NEW.scope,NEW.scope_id,NEW.role) IS DISTINCT FROM (OLD.scope,OLD.scope_id,OLD.role) THEN
            RAISE EXCEPTION 'connection scope is immutable' USING ERRCODE='23514';
        END IF;
        NEW.routing_revision:=nextval('routing_revision_generation');
        IF ROW(NEW.base_url,NEW.api_key_encrypted,NEW.enabled,NEW.connection_adapter,NEW.quota_group_id,NEW.model_metadata)
            IS DISTINCT FROM ROW(OLD.base_url,OLD.api_key_encrypted,OLD.enabled,OLD.connection_adapter,OLD.quota_group_id,OLD.model_metadata) THEN
            NEW.auth_revision:=nextval('routing_revision_generation');
        ELSE NEW.auth_revision:=OLD.auth_revision; END IF;
    ELSE
        IF NEW.role='llm' AND ROW(NEW.base_url,NEW.model,NEW.connection_adapter)
            IS DISTINCT FROM ROW(OLD.base_url,OLD.model,OLD.connection_adapter) THEN
            NEW.model_metadata:='{}'::jsonb;
        END IF;
        IF ROW(NEW.base_url,NEW.model,NEW.api_key_encrypted,NEW.enabled,NEW.reasoning_mode,NEW.connection_id,
                NEW.connection_auth_revision,NEW.connection_adapter,NEW.model_metadata,NEW.model_pricing)
            IS DISTINCT FROM ROW(OLD.base_url,OLD.model,OLD.api_key_encrypted,OLD.enabled,OLD.reasoning_mode,OLD.connection_id,
                OLD.connection_auth_revision,OLD.connection_adapter,OLD.model_metadata,OLD.model_pricing) THEN
            NEW.routing_revision:=OLD.routing_revision+1;
        ELSE NEW.routing_revision:=OLD.routing_revision; END IF;
        IF ROW(NEW.base_url,NEW.model,NEW.reasoning_mode,NEW.connection_adapter,NEW.model_metadata)
            IS DISTINCT FROM ROW(OLD.base_url,OLD.model,OLD.reasoning_mode,OLD.connection_adapter,OLD.model_metadata) THEN
            NEW.model_config_revision:=nextval('routing_revision_generation');
        ELSE NEW.model_config_revision:=OLD.model_config_revision; END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION provider_connection_propagate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    -- Legacy nested edits have exactly one consumer, updated by its BEFORE
    -- trigger. Avoid updating that same tuple recursively while it is pending.
    IF NEW.role='connection' AND pg_trigger_depth()=1 AND
        ROW(NEW.base_url,NEW.auth_revision,NEW.connection_adapter) IS DISTINCT FROM ROW(OLD.base_url,OLD.auth_revision,OLD.connection_adapter) THEN
        UPDATE provider_configs SET base_url=NEW.base_url,connection_auth_revision=NEW.auth_revision,
            connection_adapter=NEW.connection_adapter WHERE role='llm' AND connection_id=NEW.id;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER provider_connection_propagate AFTER UPDATE ON provider_configs FOR EACH ROW EXECUTE FUNCTION provider_connection_propagate();

CREATE FUNCTION provider_connection_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.role='llm' THEN
        DELETE FROM provider_configs WHERE id=OLD.connection_id AND role='connection'
            AND legacy_owner_provider_id=OLD.id AND NOT EXISTS(SELECT 1 FROM provider_configs WHERE connection_id=OLD.connection_id);
    END IF;
    RETURN OLD;
END;
$$;
CREATE TRIGGER provider_connection_cleanup AFTER DELETE ON provider_configs FOR EACH ROW EXECUTE FUNCTION provider_connection_cleanup();

CREATE TABLE model_variants (
    id UUID PRIMARY KEY,
    provider_id UUID NOT NULL REFERENCES provider_configs(id) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK(scope IN ('deployment','user')),
    scope_id UUID REFERENCES users(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK(revision>0),
    model_config_revision BIGINT NOT NULL CHECK(model_config_revision>0),
    behaviour_sha256 TEXT NOT NULL CHECK(behaviour_sha256 ~ '^[0-9a-f]{64}$'),
    variant JSONB NOT NULL CHECK(jsonb_typeof(variant)='object'),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((scope='deployment' AND scope_id IS NULL) OR (scope='user' AND scope_id IS NOT NULL))
);
CREATE UNIQUE INDEX model_variants_configuration ON model_variants(provider_id,
    COALESCE(scope_id,'00000000-0000-0000-0000-000000000000'::uuid),behaviour_sha256);

CREATE FUNCTION routing_catalogue_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE key UUID; p_scope TEXT; p_owner UUID;
BEGIN
    IF TG_TABLE_NAME='provider_configs' THEN
        IF TG_OP='DELETE' THEN p_scope:=OLD.scope; p_owner:=OLD.scope_id;
        ELSE p_scope:=NEW.scope; p_owner:=NEW.scope_id; END IF;
    ELSIF TG_TABLE_NAME='model_variants' THEN
        IF TG_OP='DELETE' THEN p_scope:=OLD.scope; p_owner:=OLD.scope_id;
        ELSE p_scope:=NEW.scope; p_owner:=NEW.scope_id; END IF;
    ELSE
        IF TG_OP='DELETE' THEN SELECT scope,scope_id INTO p_scope,p_owner FROM provider_configs WHERE id=OLD.provider_id;
        ELSE SELECT scope,scope_id INTO p_scope,p_owner FROM provider_configs WHERE id=NEW.provider_id; END IF;
    END IF;
    IF p_scope IS NULL THEN RETURN NULL; END IF;
    key:=CASE WHEN p_scope='deployment' THEN '00000000-0000-0000-0000-000000000000'::uuid ELSE p_owner END;
    IF key IS NOT NULL THEN
        INSERT INTO routing_catalogue_generations(scope_key,revision) VALUES(key,nextval('routing_revision_generation'))
            ON CONFLICT(scope_key) DO UPDATE SET revision=EXCLUDED.revision;
    END IF;
    RETURN NULL;
END;
$$;
CREATE TRIGGER provider_z_catalogue_changed AFTER INSERT OR UPDATE OR DELETE ON provider_configs FOR EACH ROW EXECUTE FUNCTION routing_catalogue_changed();
CREATE TRIGGER variant_catalogue_changed AFTER INSERT OR UPDATE OR DELETE ON model_variants FOR EACH ROW EXECUTE FUNCTION routing_catalogue_changed();
CREATE TRIGGER profile_catalogue_changed AFTER INSERT OR UPDATE OR DELETE ON model_profiles FOR EACH ROW EXECUTE FUNCTION routing_catalogue_changed();
INSERT INTO routing_catalogue_generations(scope_key,revision)
SELECT scope_key,nextval('routing_revision_generation') FROM
    (SELECT DISTINCT COALESCE(scope_id,'00000000-0000-0000-0000-000000000000'::uuid) AS scope_key FROM provider_configs) scopes
ON CONFLICT(scope_key) DO UPDATE SET revision=EXCLUDED.revision;

-- Operator maintenance only: services must be stopped and leases drained.
-- Disabling indirection first makes legacy materialisation atomic and prevents
-- the new triggers from immediately moving credentials back into connections.
CREATE FUNCTION materialise_legacy_provider_connections() RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    LOCK TABLE provider_configs IN ACCESS EXCLUSIVE MODE;
    UPDATE routing_catalogue_state SET active=false WHERE id;
    UPDATE provider_configs p SET base_url=c.base_url,api_key_encrypted=c.api_key_encrypted,
        enabled=p.enabled AND c.enabled,connection_id=NULL,connection_auth_revision=NULL
    FROM provider_configs c WHERE p.role='llm' AND p.connection_id=c.id AND c.role='connection';
    DELETE FROM provider_configs WHERE role='connection';
END;
$$;
