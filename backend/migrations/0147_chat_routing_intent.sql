-- The existing chat binding remains the sole authority. Versioned envelopes
-- need the same shape/pointer semantics as legacy selections, without dropping
-- unavailable variants/profiles into an implicit default after reload.
ALTER TABLE chats DROP CONSTRAINT chats_model_selection_check;
ALTER TABLE chats ADD CONSTRAINT chats_model_selection_check CHECK (
 jsonb_typeof(model_selection)='object' AND COALESCE(
 CASE WHEN model_selection->>'schema_version'='2' THEN
  model_selection->>'required_profile_version' IN ('1','2')
  AND jsonb_typeof(model_selection->'selection')='object'
  AND model_selection->'selection' ? 'mode'
  AND model_selection->'selection'->>'mode' IN ('default','manual','auto')
 ELSE model_selection ? 'mode' AND model_selection->>'mode' IN ('default','manual','auto') END,false)
);
CREATE OR REPLACE FUNCTION sync_chat_model_selection() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE selected JSONB;
BEGIN
 IF NEW.model_selection IS DISTINCT FROM OLD.model_selection THEN
  selected:=CASE WHEN NEW.model_selection->>'schema_version'='2' THEN NEW.model_selection->'selection' ELSE NEW.model_selection END;
  NEW.llm_provider_id:=CASE WHEN selected->>'mode'='manual' THEN (selected->>'provider_id')::uuid ELSE NULL END;
 ELSIF NEW.llm_provider_id IS DISTINCT FROM OLD.llm_provider_id AND OLD.model_selection->>'schema_version' IS DISTINCT FROM '2' THEN
  NEW.model_selection:=CASE WHEN NEW.llm_provider_id IS NULL THEN '{"mode":"default"}'::jsonb
   ELSE jsonb_build_object('mode','manual','provider_id',NEW.llm_provider_id) END;
 END IF;
 RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION insert_chat_model_selection() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE selected JSONB;
BEGIN
 selected:=CASE WHEN NEW.model_selection->>'schema_version'='2' THEN NEW.model_selection->'selection' ELSE NEW.model_selection END;
 IF selected->>'mode'='manual' THEN
  IF selected->>'provider_id' IS NULL THEN RAISE EXCEPTION 'manual selection requires provider_id' USING ERRCODE='23514'; END IF;
  IF NEW.llm_provider_id IS NOT NULL AND NEW.llm_provider_id<>(selected->>'provider_id')::uuid THEN
   RAISE EXCEPTION 'conflicting manual selection and provider pointer' USING ERRCODE='23514';
  END IF;
  NEW.llm_provider_id:=(selected->>'provider_id')::uuid;
 ELSIF selected->>'mode'='auto' OR NEW.model_selection->>'schema_version'='2' THEN
  NEW.llm_provider_id:=NULL;
 ELSIF NEW.llm_provider_id IS NOT NULL THEN
  NEW.model_selection:=jsonb_build_object('mode','manual','provider_id',NEW.llm_provider_id);
 END IF;
 RETURN NEW;
END $$;
