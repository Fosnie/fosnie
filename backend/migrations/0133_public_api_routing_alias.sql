-- Core-owned, opt-in alias. Existing provider labels keep their meaning.
CREATE TABLE routing_api_alias (
    id SMALLINT PRIMARY KEY CHECK (id=1),
    name TEXT NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$' AND name NOT LIKE 'agent/%'),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    objective TEXT NOT NULL DEFAULT 'balanced' CHECK (objective IN ('economy','balanced','quality')),
    revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation'),
    -- Every provider namespace mutation writes this row too. Thus concurrent
    -- activation cannot miss a provider even at repeatable-read isolation.
    namespace_epoch BIGINT NOT NULL DEFAULT 0,
    updated_by UUID REFERENCES users(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO routing_api_alias(id,name) VALUES(1,'fosnie/auto');

CREATE FUNCTION routing_api_alias_validate() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.enabled AND EXISTS (
        SELECT 1 FROM provider_configs p WHERE p.role='llm'
          AND COALESCE(p.label,p.model,p.id::text)=NEW.name
    ) THEN
        RAISE EXCEPTION 'routing alias conflicts with an existing provider name'
            USING ERRCODE='23505', CONSTRAINT='routing_api_alias_name_collision';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER routing_api_alias_validate BEFORE INSERT OR UPDATE OF name,enabled,objective
    ON routing_api_alias FOR EACH ROW EXECUTE FUNCTION routing_api_alias_validate();

CREATE FUNCTION routing_api_alias_provider_namespace() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE reserved_name TEXT; reserved_enabled BOOLEAN;
BEGIN
    IF TG_OP='DELETE' THEN
        IF OLD.role<>'llm' THEN RETURN OLD; END IF;
    ELSIF TG_OP='UPDATE' THEN
        IF NEW.role<>'llm' AND OLD.role<>'llm' THEN RETURN NEW; END IF;
    ELSIF NEW.role<>'llm' THEN RETURN NEW;
    END IF;
    UPDATE routing_api_alias a SET namespace_epoch=a.namespace_epoch+1 WHERE a.id=1
        RETURNING a.name,a.enabled INTO reserved_name,reserved_enabled;
    IF TG_OP<>'DELETE' AND NEW.role='llm' AND reserved_enabled
        AND COALESCE(NEW.label,NEW.model,NEW.id::text)=reserved_name THEN
        RAISE EXCEPTION 'provider name is reserved by the enabled routing alias'
            USING ERRCODE='23505', CONSTRAINT='routing_api_alias_name_collision';
    END IF;
    IF TG_OP='DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER routing_api_alias_provider_namespace BEFORE INSERT OR UPDATE OR DELETE
    ON provider_configs FOR EACH ROW EXECUTE FUNCTION routing_api_alias_provider_namespace();
