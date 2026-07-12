-- Copyright 2026 Private AI Ltd (SC881079)
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.

-- 0078_scim_tokens.sql — named bearer tokens authenticating a customer IdP's SCIM
-- provisioning engine (Enterprise SSO/SCIM). These are the
-- long-lived secrets Okta ("Header Auth"/OAuth) and Entra ("Secret Token") send on
-- every SCIM request. Unlike the operational break-glass token (opaque value held
-- as a short-TTL Redis key), a SCIM token is durable and administered, so:
--
--   * token_hash — the SHA-256 (hex) of a 256-bit CSPRNG secret. The plaintext is
--     shown ONCE at creation and never stored; lookup is by hash.
--   * prefix     — the first 8 characters of the secret, kept in clear purely to
--     identify a token in the admin UI ("which one shall I revoke?").
--   * expires_at — NULL = non-expiring (Entra prefers long-lived tokens; the docs
--     recommend 180-day rotation). A non-NULL value in the past = expired.
--   * revoked_at — soft revocation; a revoked or expired token authenticates nothing.
--
-- Enterprise-only in practice (the SCIM server lives in the private edition), but
-- Core schema under the unified-migrations rule; empty and inert in a Core deploy.

CREATE TABLE scim_tokens (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    label        TEXT        NOT NULL,
    token_hash   TEXT        NOT NULL UNIQUE,       -- SHA-256 hex of the secret
    prefix       TEXT        NOT NULL,              -- first 8 chars, for identification
    expires_at   TIMESTAMPTZ,                       -- NULL = non-expiring
    last_used_at TIMESTAMPTZ,
    created_by   UUID        REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at   TIMESTAMPTZ
);

-- Hot path: authenticate an incoming SCIM request by its hashed bearer token.
CREATE INDEX scim_tokens_hash_idx ON scim_tokens (token_hash);
