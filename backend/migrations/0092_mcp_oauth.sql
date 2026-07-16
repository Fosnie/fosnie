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

-- 0092_mcp_oauth.sql — one-click MCP connections: OAuth 2.1 (PKCE + Dynamic Client
-- Registration + RFC 8707 resource indicators) for remote MCP servers. Lets an admin
-- register a remote server by URL only, discover its authorisation server, and (where
-- the AS supports DCR) register a client automatically; where it does not, the admin
-- pastes a client_id once per server. Each user then completes their own OAuth flow, so
-- the server's tools run under that user's own identity at the provider rather than a
-- single shared bearer token.
--
-- Two new tables. `mcp_oauth_clients` is our OAuth *client* registration at one
-- authorisation server AND the admin's persisted approval of that issuer: the validated
-- AS metadata stored here is the runtime source of truth — the connect path never
-- re-discovers, so a server that later changes its WWW-Authenticate cannot move us.
-- `mcp_oauth_connections` holds one principal's encrypted tokens at one server (user_id
-- NULL = an admin-owned service connection for unattended runs). Secrets live only in
-- the `*_enc` columns (AES-256-GCM with the deployment message key, like mcp auth /
-- connector tokens); plaintext never touches the DB. Forward-only; owned by sqlx-cli.
--
-- Upgrade is a behavioural no-op: existing rows keep their auth_type and nothing reads
-- these tables until an admin opts a server in to 'oauth'.

-- Extend the existing single-column auth_type check to admit 'oauth'. Drop + recreate
-- because a CHECK constraint cannot be altered in place.
ALTER TABLE mcp_servers DROP CONSTRAINT mcp_servers_auth_type_chk;
ALTER TABLE mcp_servers
    ADD CONSTRAINT mcp_servers_auth_type_chk
    CHECK (auth_type IN ('none', 'bearer', 'api_key', 'header', 'oauth'));

-- A stdio server has no HTTP 401 to discover an authorisation server from, so it can
-- never do OAuth. Separate cross-column constraint (the one above is single-column).
ALTER TABLE mcp_servers
    ADD CONSTRAINT mcp_servers_oauth_http_only_chk
    CHECK (NOT (auth_type = 'oauth' AND transport = 'stdio'));

-- Our OAuth client registration at one AS + the admin's persisted approval of that
-- issuer. Keyed by (server, issuer): issuer-keying is required, and an AS swap must
-- force re-approval rather than silently reuse a client minted for the old issuer.
CREATE TABLE mcp_oauth_clients (
    id                            UUID        PRIMARY KEY,
    mcp_server_id                 UUID        NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    issuer                        TEXT        NOT NULL,
    client_id                     TEXT        NOT NULL,
    client_secret_enc             TEXT,                              -- NULL = public client (PKCE only)
    registration_source           TEXT        NOT NULL,             -- manual | dcr
    registration_client_uri       TEXT,                             -- RFC 7592 management endpoint (DCR only)
    registration_access_token_enc TEXT,                             -- RFC 7592 management token, encrypted
    scopes                        TEXT[]      NOT NULL DEFAULT '{}',
    metadata                      JSONB       NOT NULL,             -- validated, admin-approved AS metadata (runtime source of truth)
    approved_by                   UUID        REFERENCES users(id),
    approved_at                   TIMESTAMPTZ NOT NULL,
    created_at                    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT mcp_oauth_clients_source_chk
        CHECK (registration_source IN ('manual', 'dcr'))
);
CREATE UNIQUE INDEX mcp_oauth_clients_uniq ON mcp_oauth_clients (mcp_server_id, issuer);

-- One principal's tokens at one server. A row is minted 'pending' at connect time
-- (before any token exists) and flipped to 'active' on a successful code exchange; the
-- CHECK forbids an active row without a token. Revoke clears the ciphertext outright
-- (it does not merely flag), matching the connector precedent.
CREATE TABLE mcp_oauth_connections (
    id                UUID        PRIMARY KEY,
    mcp_server_id     UUID        NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    oauth_client_id   UUID        NOT NULL REFERENCES mcp_oauth_clients(id) ON DELETE CASCADE,
    user_id           UUID        REFERENCES users(id) ON DELETE CASCADE,  -- NULL = service connection (unattended runs)
    subject_label     TEXT,                                                -- provider account display, where derivable
    access_token_enc  TEXT,
    refresh_token_enc TEXT,
    expires_at        TIMESTAMPTZ,                                         -- nullable: a pending row precedes any token
    scopes            TEXT[]      NOT NULL DEFAULT '{}',
    status            TEXT        NOT NULL DEFAULT 'pending',
    is_catalog_source BOOLEAN     NOT NULL DEFAULT false,                  -- the one connection the health sweep pins against
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at      TIMESTAMPTZ,
    CONSTRAINT mcp_oauth_connections_status_chk
        CHECK (status IN ('pending', 'active', 'reauth_required', 'revoked')),
    CONSTRAINT mcp_oauth_connections_active_has_token_chk
        CHECK (status <> 'active' OR access_token_enc IS NOT NULL)
);
-- One connection per user per server, and a single service connection per server. NULLs
-- are distinct in a unique index, so the service form (user_id NULL) needs its own
-- partial unique. And at most one catalogue source per server.
CREATE UNIQUE INDEX mcp_oauth_connections_user_uniq
    ON mcp_oauth_connections (mcp_server_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX mcp_oauth_connections_service_uniq
    ON mcp_oauth_connections (mcp_server_id) WHERE user_id IS NULL;
CREATE UNIQUE INDEX mcp_oauth_connections_catalog_uniq
    ON mcp_oauth_connections (mcp_server_id) WHERE is_catalog_source;
CREATE INDEX mcp_oauth_connections_user_idx ON mcp_oauth_connections (user_id);
