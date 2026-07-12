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

-- 0006_skills_prompts_memory.sql — Skills, Prompts, Memory. Pointer-in-DB + content-on-disk
-- for skills/prompts; explicit-only memory facts. Forward-only; owned by sqlx-cli.

-- Skills: instruction modules (open Agent Skills standard) attached to Agents.
CREATE TABLE skills (
    id          UUID        PRIMARY KEY,
    name        TEXT        NOT NULL,
    description TEXT        NOT NULL,
    disk_path   TEXT        NOT NULL,                 -- skill folder (SKILL.md inside)
    scope       TEXT        NOT NULL DEFAULT 'personal',
    created_by  UUID        REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE agent_skills (
    agent_id UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    skill_id UUID NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    PRIMARY KEY (agent_id, skill_id)
);

-- Prompts: Markdown templates with placeholders; content on disk, pointer here.
CREATE TABLE prompts (
    id           UUID        PRIMARY KEY,
    name         TEXT        NOT NULL,
    content_path TEXT        NOT NULL,
    scope        TEXT        NOT NULL DEFAULT 'personal',  -- personal | project | global
    project_id   UUID        REFERENCES projects(id),
    created_by   UUID        REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX prompts_created_by_idx ON prompts (created_by);

-- Memory: explicit-only facts (explicit adds plus pinned entries).
CREATE TYPE mem_scope AS ENUM ('user', 'project');

CREATE TABLE memory_facts (
    id               UUID        PRIMARY KEY,
    scope            mem_scope   NOT NULL,
    owner_user_id    UUID        REFERENCES users(id) ON DELETE CASCADE,    -- scope=user
    owner_project_id UUID        REFERENCES projects(id) ON DELETE CASCADE, -- scope=project
    content          TEXT        NOT NULL,
    source_ref       UUID,                                  -- chat the fact came from
    pinned           BOOLEAN     NOT NULL DEFAULT false,
    user_edited      BOOLEAN     NOT NULL DEFAULT false,
    created_by       UUID        REFERENCES users(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (scope = 'user'    AND owner_user_id    IS NOT NULL AND owner_project_id IS NULL) OR
        (scope = 'project' AND owner_project_id IS NOT NULL AND owner_user_id    IS NULL)
    )
);
CREATE INDEX memory_facts_user_idx ON memory_facts (owner_user_id) WHERE owner_user_id IS NOT NULL;
CREATE INDEX memory_facts_project_idx ON memory_facts (owner_project_id) WHERE owner_project_id IS NOT NULL;
