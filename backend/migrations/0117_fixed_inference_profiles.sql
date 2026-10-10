-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Fixed roles are attested separately from quality-ranked LLM profiles.
CREATE TABLE fixed_inference_profiles (
    provider_id UUID PRIMARY KEY REFERENCES provider_configs(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    profile JSONB NOT NULL CHECK (jsonb_typeof(profile) = 'object'),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE embedding_index ADD COLUMN routing_revision BIGINT NOT NULL
    DEFAULT nextval('routing_revision_generation');

CREATE FUNCTION bump_active_embed_routing_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.embed_model, NEW.embed_base_url, NEW.embed_api_key_encrypted, NEW.dim, NEW.collection_name)
       IS DISTINCT FROM ROW(OLD.embed_model, OLD.embed_base_url, OLD.embed_api_key_encrypted, OLD.dim, OLD.collection_name) THEN
        NEW.routing_revision := nextval('routing_revision_generation');
    ELSE
        NEW.routing_revision := OLD.routing_revision;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER active_embed_routing_revision BEFORE UPDATE ON embedding_index
    FOR EACH ROW EXECUTE FUNCTION bump_active_embed_routing_revision();
