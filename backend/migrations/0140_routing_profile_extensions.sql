-- One policy identity/revision and one default authority; legacy JSON stays v1.
ALTER TABLE routing_policies ADD COLUMN profile_config JSONB;
ALTER TABLE routing_policies ADD CONSTRAINT routing_profile_version CHECK
 (profile_config IS NULL OR (jsonb_typeof(profile_config)='object' AND profile_config->>'schema_version'='2'));
-- A project binding is shared intent, never a stored actor's credentials.
ALTER TABLE projects ADD COLUMN routing_intent JSONB;
ALTER TABLE projects ADD COLUMN routing_revision BIGINT NOT NULL DEFAULT nextval('routing_revision_generation');
ALTER TABLE projects ADD CONSTRAINT project_routing_intent_shape CHECK
 (routing_intent IS NULL OR (jsonb_typeof(routing_intent)='object' AND routing_intent->>'schema_version'='2'));
