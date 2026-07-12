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

-- 0088_tool_overrides.sql — per-deployment overrides for the native tool
-- registry. Until now the 12 built-in tools
-- (`tools::ALL`) were fully closed: an admin could not switch one off or edit
-- the description the LLM sees. This table holds an optional override row per
-- native tool name: `enabled=false` is a kill-switch (the tool is dropped from
-- the per-turn tool defs and the agent editor, like a dormant connector);
-- `description_override` replaces the schema description advertised to the model
-- (real behaviour customisation without a fork). `tool_name` is validated
-- app-side against `tools::ALL` (no FK — the registry is code, not data, per
-- ТЗ D6). An empty table keeps every tool byte-identical to the code default,
-- which the prefix-cache relies on. Forward-only.
CREATE TABLE tool_overrides (
    tool_name            TEXT PRIMARY KEY,
    enabled              BOOLEAN NOT NULL DEFAULT true,
    description_override  TEXT,
    updated_by           UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
