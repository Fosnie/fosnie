-- Agent version history (regulated auditability).
-- Agents are edited in place; this records an immutable snapshot of the Agent's
-- configuration on every create / update / rollback, so the history is kept, a
-- prior version can be restored, and an answer can be tied to the exact Agent
-- version that produced it (stamped into the chat-turn audit event).
--
-- Snapshot, not diff (mirrors document_versions): the core fields plus the
-- tool-set and Project-Knowledge scope as JSONB arrays, so a rollback restores
-- the whole config without reconstruction. Prompts are immutable by design (each
-- is its own version), so only Agents need this.
CREATE TABLE agent_versions (
    id                    UUID        PRIMARY KEY,
    agent_id              UUID        NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    version_number        INT         NOT NULL,
    source                TEXT        NOT NULL,  -- 'created' | 'updated' | 'rollback'
    name                  TEXT        NOT NULL,
    description           TEXT,
    system_prompt         TEXT        NOT NULL,
    params                JSONB       NOT NULL DEFAULT '{}'::jsonb,
    tools                 JSONB       NOT NULL DEFAULT '[]'::jsonb,  -- snapshot of tool names
    project_knowledge_ids JSONB       NOT NULL DEFAULT '[]'::jsonb,  -- snapshot of PK ids (as text)
    created_by            UUID        REFERENCES users(id),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (agent_id, version_number)
);

CREATE INDEX agent_versions_agent_idx ON agent_versions (agent_id, version_number DESC);

-- Backfill v1 for every existing Agent so each has a baseline history entry.
INSERT INTO agent_versions
    (id, agent_id, version_number, source, name, description, system_prompt, params,
     tools, project_knowledge_ids, created_by, created_at)
SELECT gen_random_uuid(), a.id, 1, 'created', a.name, a.description, a.system_prompt, a.params,
       COALESCE((SELECT jsonb_agg(tool_name ORDER BY tool_name)
                 FROM agent_tools WHERE agent_id = a.id), '[]'::jsonb),
       COALESCE((SELECT jsonb_agg(project_knowledge_id::text)
                 FROM agent_project_knowledge WHERE agent_id = a.id), '[]'::jsonb),
       a.created_by, a.created_at
FROM agents a;
