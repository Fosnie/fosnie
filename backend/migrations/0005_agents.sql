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

-- 0005_agents.sql — Agents (the named LLM configuration) + bindings
-- The Agent is the central config object
-- referenced by tools, skills, prompts, RAG scope, tabular, feedback,
-- automations. Forward-only; owned by sqlx-cli.

CREATE TABLE agents (
    id            UUID        PRIMARY KEY,
    name          TEXT        NOT NULL,
    description   TEXT,
    system_prompt TEXT        NOT NULL,
    params        JSONB       NOT NULL DEFAULT '{}'::jsonb,  -- temperature, top_p, max_tokens, tool_concurrency, top_k
    created_by    UUID        REFERENCES users(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at   TIMESTAMPTZ
);
CREATE INDEX agents_created_by_idx ON agents (created_by);

-- Which closed-set tools an Agent may call (tool names are platform-defined).
CREATE TABLE agent_tools (
    agent_id  UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    PRIMARY KEY (agent_id, tool_name)
);

-- Which Project Knowledge bases an Agent may retrieve from (RAG scope).
CREATE TABLE agent_project_knowledge (
    agent_id             UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    project_knowledge_id UUID NOT NULL REFERENCES project_knowledge(id) ON DELETE CASCADE,
    PRIMARY KEY (agent_id, project_knowledge_id)
);

-- chats.agent_id was laid in nullable without an FK (0003); bind it now.
ALTER TABLE chats
    ADD CONSTRAINT chats_agent_id_fkey
    FOREIGN KEY (agent_id) REFERENCES agents(id);
