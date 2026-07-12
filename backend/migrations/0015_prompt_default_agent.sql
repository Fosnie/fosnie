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

-- 0015_prompt_default_agent.sql — Optional default Agent on a Prompt
-- When a Prompt names a default Agent, the client uses it
-- to pre-select which Agent the rendered prompt is sent to; null = no default.
-- Mirrors the automations.agent_id nullable-FK pattern. Forward-only; owned by
-- sqlx-cli.

ALTER TABLE prompts ADD COLUMN agent_id UUID REFERENCES agents(id);
