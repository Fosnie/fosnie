-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Existing profiles remain the default stage of the same provider.
ALTER TABLE fixed_inference_profiles ADD COLUMN slot TEXT;
UPDATE fixed_inference_profiles f SET slot=p.role FROM provider_configs p WHERE p.id=f.provider_id;
ALTER TABLE fixed_inference_profiles ALTER COLUMN slot SET NOT NULL;
ALTER TABLE fixed_inference_profiles ADD CHECK (slot IN ('embed','embed_desired','rerank','ocr','stt','tts','verify','verify_claims'));
ALTER TABLE fixed_inference_profiles DROP CONSTRAINT fixed_inference_profiles_pkey;
ALTER TABLE fixed_inference_profiles ADD PRIMARY KEY(provider_id,slot);

ALTER TABLE embedding_index ADD COLUMN desired_routing_revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation');
CREATE FUNCTION bump_desired_embed_routing_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.desired_model,NEW.desired_base_url,NEW.desired_api_key_encrypted,NEW.desired_dim,NEW.desired_collection_name)
       IS DISTINCT FROM ROW(OLD.desired_model,OLD.desired_base_url,OLD.desired_api_key_encrypted,OLD.desired_dim,OLD.desired_collection_name) THEN
        NEW.desired_routing_revision := nextval('routing_revision_generation');
    ELSE
        NEW.desired_routing_revision := OLD.desired_routing_revision;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER desired_embed_routing_revision BEFORE UPDATE ON embedding_index
    FOR EACH ROW EXECUTE FUNCTION bump_desired_embed_routing_revision();
