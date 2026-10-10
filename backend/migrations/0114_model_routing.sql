-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Additive, default-off Core routing. No provider credentials are duplicated.

ALTER TABLE provider_configs ADD COLUMN routing_revision BIGINT NOT NULL DEFAULT 1;

CREATE FUNCTION bump_provider_routing_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.base_url, NEW.model, NEW.api_key_encrypted, NEW.enabled, NEW.reasoning_mode)
       IS DISTINCT FROM ROW(OLD.base_url, OLD.model, OLD.api_key_encrypted, OLD.enabled, OLD.reasoning_mode) THEN
        NEW.routing_revision := OLD.routing_revision + 1;
    ELSE
        NEW.routing_revision := OLD.routing_revision;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER provider_routing_revision BEFORE UPDATE ON provider_configs
    FOR EACH ROW EXECUTE FUNCTION bump_provider_routing_revision();

CREATE TABLE model_profiles (
    provider_id UUID PRIMARY KEY REFERENCES provider_configs(id) ON DELETE CASCADE,
    provider_revision BIGINT NOT NULL CHECK (provider_revision > 0),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    profile JSONB NOT NULL CHECK (jsonb_typeof(profile) = 'object'),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE routing_policies (
    id UUID PRIMARY KEY,
    label TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 120),
    scope TEXT NOT NULL CHECK (scope IN ('deployment', 'user')),
    scope_id UUID REFERENCES users(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    is_default BOOLEAN NOT NULL DEFAULT false,
    policy JSONB NOT NULL CHECK (jsonb_typeof(policy) = 'object'),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((scope = 'deployment' AND scope_id IS NULL) OR (scope = 'user' AND scope_id IS NOT NULL))
);
CREATE UNIQUE INDEX routing_policy_default ON routing_policies
    (scope, COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid)) WHERE is_default;

ALTER TABLE chats ADD COLUMN model_selection JSONB NOT NULL DEFAULT '{"mode":"default"}'::jsonb
    CHECK (jsonb_typeof(model_selection) = 'object'
       AND model_selection ? 'mode'
       AND model_selection->>'mode' IN ('default', 'manual', 'auto'));
UPDATE chats SET model_selection = jsonb_build_object('mode', 'manual', 'provider_id', llm_provider_id)
    WHERE llm_provider_id IS NOT NULL;

-- Existing clients/handlers can still update the legacy pointer. New selection
-- updates explicitly clear it for Auto/default, without interpreting old nulls
-- on an ordinary chat.send as a reset (those do not update the pointer).
CREATE FUNCTION sync_chat_model_selection() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.model_selection IS DISTINCT FROM OLD.model_selection THEN
        IF NEW.model_selection->>'mode' = 'manual' THEN
            NEW.llm_provider_id := (NEW.model_selection->>'provider_id')::uuid;
            IF NEW.llm_provider_id IS NULL THEN
                RAISE EXCEPTION 'manual selection requires provider_id' USING ERRCODE = '23514';
            END IF;
        ELSE
            NEW.llm_provider_id := NULL;
        END IF;
    ELSIF NEW.llm_provider_id IS DISTINCT FROM OLD.llm_provider_id THEN
        NEW.model_selection := CASE WHEN NEW.llm_provider_id IS NULL THEN '{"mode":"default"}'::jsonb
            ELSE jsonb_build_object('mode', 'manual', 'provider_id', NEW.llm_provider_id) END;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER chat_model_selection BEFORE UPDATE OF model_selection, llm_provider_id ON chats
    FOR EACH ROW EXECUTE FUNCTION sync_chat_model_selection();

-- Metadata lives in the existing lifecycle-managed chat, not a new raw-prompt log.
ALTER TABLE messages ADD COLUMN routing_decision JSONB;
