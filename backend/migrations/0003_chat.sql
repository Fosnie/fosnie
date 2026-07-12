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

-- 0003_chat.sql — chats + messages for the first chat turn (schema §5).
--
-- No RAG yet: citations and chat_shares are deferred. project_id/agent_id are
-- nullable with no FK (projects/agents tables land in later slices), matching
-- the skeleton's forward-reference pattern. Forward-only; owned by sqlx-cli.

CREATE TYPE message_role AS ENUM ('user', 'assistant', 'system', 'tool');

-- A chat lives inside a Project (nullable = general/personal). Flat, no tree.
CREATE TABLE chats (
    id            UUID        PRIMARY KEY,
    project_id    UUID,                         -- FK projects(id) later
    agent_id      UUID,                         -- FK agents(id) later
    owner_user_id UUID        NOT NULL REFERENCES users(id),
    title         TEXT        NOT NULL DEFAULT 'New chat',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at   TIMESTAMPTZ
);

CREATE INDEX chats_owner_idx ON chats (owner_user_id);

-- A linear sequence of messages in one chat (no branching tree).
CREATE TABLE messages (
    id                UUID         PRIMARY KEY,
    chat_id           UUID         NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    role              message_role NOT NULL,
    sequence_number   INT          NOT NULL,
    content           TEXT         NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    completed_at      TIMESTAMPTZ,               -- assistant: set on success
    interrupted_at    TIMESTAMPTZ,               -- assistant: set on cancel/drop
    prompt_tokens     INT,
    completion_tokens INT,
    UNIQUE (chat_id, sequence_number)
);

CREATE INDEX messages_chat_seq_idx ON messages (chat_id, sequence_number);
