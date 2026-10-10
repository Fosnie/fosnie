-- Safe failure/attempt observations; no credentials or vendor response bodies.
ALTER TABLE messages ADD COLUMN IF NOT EXISTS routing_failure JSONB;
ALTER TABLE messages ADD COLUMN IF NOT EXISTS routing_attempts JSONB;
CREATE TABLE routing_provider_health (
    provider_id UUID NOT NULL REFERENCES provider_configs(id) ON DELETE CASCADE,
    provider_revision BIGINT NOT NULL,
    profile_revision BIGINT NOT NULL,
    purpose TEXT NOT NULL,
    failure JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(provider_id,provider_revision,profile_revision,purpose)
);
CREATE INDEX routing_provider_health_expiry ON routing_provider_health(expires_at);
CREATE TABLE routing_credential_bindings (
    provider_id UUID PRIMARY KEY REFERENCES provider_configs(id) ON DELETE CASCADE,
    provider_revision BIGINT NOT NULL,
    binding_hash TEXT NOT NULL
);
CREATE TABLE routing_credential_health (
    binding_hash TEXT PRIMARY KEY,
    source_provider_id UUID NOT NULL REFERENCES provider_configs(id) ON DELETE CASCADE,
    source_revision BIGINT NOT NULL,
    failure JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
