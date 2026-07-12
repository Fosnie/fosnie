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

-- 0072_mcp_auth.sql — per-server auth + remote-egress flag for MCP servers (FEATURE B1
-- activation). Lets an admin register a remote (public HTTPS) MCP server — GitHub,
-- Cloudflare, Context7 — that authenticates with a bearer token / API key / custom
-- header, injected on every request to that server. The secret is stored ENCRYPTED
-- (AES-256-GCM with the deployment message key, like dm_bodies / provider api-keys);
-- the plaintext never touches the DB. `requires_egress` marks a server as reaching a
-- public/remote endpoint: it lifts the private-only URL guard (SSRF hardening stays —
-- cloud-metadata + link-local are still refused, https is mandatory) and is surfaced
-- in the admin UI + egress audit. Zero-egress default is preserved: a server flows no
-- traffic until MCP is enabled platform-wide (integration.mcp.enabled) AND the server
-- is approved (enabled = true).

ALTER TABLE mcp_servers
    ADD COLUMN auth_type        TEXT    NOT NULL DEFAULT 'none',  -- none | bearer | api_key | header
    ADD COLUMN auth_header_name TEXT,                              -- custom header name (default 'Authorization')
    ADD COLUMN auth_value_enc   TEXT,                              -- encrypted secret (bearer/api-key/header value)
    ADD COLUMN requires_egress  BOOLEAN NOT NULL DEFAULT false;    -- true ⇒ remote/public endpoint (egress)

ALTER TABLE mcp_servers
    ADD CONSTRAINT mcp_servers_auth_type_chk
    CHECK (auth_type IN ('none', 'bearer', 'api_key', 'header'));
