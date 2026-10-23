-- Private dispatch provenance, never another credential or retry store.
-- Keep old scopes for bounded in-flight callbacks after rotation/retirement.
CREATE TABLE routing_health_bindings (
 provider_id UUID NOT NULL,
 provider_revision BIGINT NOT NULL CHECK(provider_revision>0),
 scope_keys JSONB NOT NULL CHECK(jsonb_typeof(scope_keys)='array' AND jsonb_array_length(scope_keys) BETWEEN 1 AND 4),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(provider_id,provider_revision)
);
CREATE INDEX routing_health_bindings_created ON routing_health_bindings(created_at);
CREATE FUNCTION routing_health_binding_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
 BEGIN RAISE EXCEPTION 'health dispatch scopes are immutable' USING ERRCODE='23514'; END $$;
CREATE TRIGGER routing_health_binding_immutable BEFORE UPDATE ON routing_health_bindings
 FOR EACH ROW EXECUTE FUNCTION routing_health_binding_immutable();
