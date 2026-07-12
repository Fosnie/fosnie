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

-- 0089_custom_tools.sql — deployment-defined custom tools (http and script
-- kinds). A custom tool is a declarative call
-- an admin registers, versions and approves; an Agent may then select it by name
-- (via the existing `agent_tools` list) and call it in the agentic loop. The name
-- is three-way disjoint from the other tool namespaces: native tools live in the
-- code registry (`tools::ALL`), MCP tools are namespaced `slug__tool` (contain
-- `__`), and a custom name may be neither — the CHECK below forbids `__`, and the
-- application additionally rejects any name equal to a native tool. Secrets ride
-- `auth_value_enc` (keyring `encrypt_at_rest`, never returned). Anti-rug-pull
-- (ТЗ D5): every edit bumps `version` and clears `approved_version`, so the loop
-- only advertises/dispatches a tool while `approved_version = version`. Forward-only.
CREATE TABLE custom_tools (
    id                UUID PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE
                        CONSTRAINT custom_tools_name_no_delim CHECK (name NOT LIKE '%\_\_%'),
    display_name      TEXT NOT NULL,
    description       TEXT NOT NULL,
    kind              TEXT NOT NULL
                        CONSTRAINT custom_tools_kind_chk CHECK (kind IN ('http', 'script')),
    -- OpenAI-format `parameters` JSON-Schema advertised to the model.
    params_schema     JSONB NOT NULL,
    -- http: {method, url, headers{}, body?, auth{type,header_name?}, response{mode,pointer?}}
    -- script (Slice C): {source}
    config            JSONB NOT NULL,
    auth_value_enc    TEXT,
    -- Dual-mode SSRF (mirrors mcp_servers): false ⇒ endpoint must resolve private;
    -- true ⇒ may reach a public HTTPS host. Ignored for the script kind.
    requires_egress   BOOLEAN NOT NULL DEFAULT true,
    -- true ⇒ the call takes a human-approval gate before it dispatches.
    side_effecting    BOOLEAN NOT NULL DEFAULT true,
    enabled           BOOLEAN NOT NULL DEFAULT false,
    -- The version an admin approved; the tool is live only while this = version.
    approved_version  INT,
    version           INT NOT NULL DEFAULT 1,
    timeout_secs      INT,
    created_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Immutable per-version snapshots — history + diff for the audit trail (ТЗ D5).
CREATE TABLE custom_tool_versions (
    id          UUID PRIMARY KEY,
    tool_id     UUID NOT NULL REFERENCES custom_tools(id) ON DELETE CASCADE,
    version     INT NOT NULL,
    snapshot    JSONB NOT NULL,
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tool_id, version)
);
