-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Legacy clients may create chats with the provider pointer already populated.
CREATE FUNCTION insert_chat_model_selection() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.model_selection->>'mode' = 'manual' THEN
        IF NEW.model_selection->>'provider_id' IS NULL THEN
            RAISE EXCEPTION 'manual selection requires provider_id' USING ERRCODE = '23514';
        END IF;
        IF NEW.llm_provider_id IS NOT NULL AND NEW.llm_provider_id <> (NEW.model_selection->>'provider_id')::uuid THEN
            RAISE EXCEPTION 'conflicting model selections' USING ERRCODE = '23514';
        END IF;
        NEW.llm_provider_id := (NEW.model_selection->>'provider_id')::uuid;
    ELSIF NEW.model_selection->>'mode' = 'auto' THEN
        IF NEW.llm_provider_id IS NOT NULL THEN
            RAISE EXCEPTION 'conflicting model selections' USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.llm_provider_id IS NOT NULL THEN
        NEW.model_selection := jsonb_build_object('mode', 'manual', 'provider_id', NEW.llm_provider_id);
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER chat_model_selection_insert BEFORE INSERT ON chats
    FOR EACH ROW EXECUTE FUNCTION insert_chat_model_selection();
