-- Scope health is shared across workers; digests never leave the private store.
CREATE TABLE routing_health_scopes (
 scope_hash TEXT PRIMARY KEY CHECK(scope_hash ~ '^[0-9a-f]{64}$'),
 kind TEXT NOT NULL CHECK(kind IN ('model','connection','credential','quota_group')),
 failure JSONB NOT NULL,expires_at TIMESTAMPTZ NOT NULL,
 probe_token UUID,probe_until TIMESTAMPTZ,
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK((probe_token IS NULL)=(probe_until IS NULL))
);
CREATE INDEX routing_health_scopes_expiry ON routing_health_scopes(expires_at);
