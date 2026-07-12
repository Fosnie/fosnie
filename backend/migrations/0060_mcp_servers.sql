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
-- FEATURE B1 — native MCP tool support: admin-registered server registry.
--
-- The platform is an MCP *client/host*. Each row is an admin-approved, allow-listed
-- server (client-internal only). Tools are namespaced `slug__toolName` (slug must not
-- contain the `__` delimiter). `pinned_tools` fingerprints the approved catalog so a
-- reconnect whose description/schema differs is auto-quarantined (rug-pull defence).
-- Per-principal access is governed by AccessGrants (resource_type 'mcp_server', 0061),
-- not columns here. Egress stays gated by the dormant-by-default `integration.mcp.enabled`.

CREATE TABLE mcp_servers (
    id             UUID        PRIMARY KEY,
    slug           TEXT        NOT NULL UNIQUE,           -- namespace prefix
    name           TEXT        NOT NULL,
    transport      TEXT        NOT NULL,                  -- 'stdio' | 'http'
    command        JSONB,                                 -- stdio: ["bin","arg",…]
    url            TEXT,                                  -- http: private endpoint
    status         TEXT        NOT NULL DEFAULT 'pending',-- pending|active|quarantined|unreachable
    enabled        BOOLEAN     NOT NULL DEFAULT false,
    pinned_tools   JSONB,                                 -- {toolName: fingerprint_hex} at approval
    tools_catalog  JSONB,                                 -- [{name,description,schema,side_effecting}]
    created_by     UUID        REFERENCES users(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_health_at TIMESTAMPTZ,
    -- The slug is the tool namespace; it must not contain the `__` delimiter.
    CONSTRAINT mcp_servers_slug_no_delim CHECK (slug NOT LIKE '%\_\_%')
);

CREATE INDEX mcp_servers_status_idx ON mcp_servers (status);
