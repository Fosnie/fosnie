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

-- 0011_team_messaging.sql — In-platform team messaging.
-- Direct/group/project chats + members + reliable messages +
-- shared notes. Separate from the LLM `chats`/`messages` tables by design.
-- Reliable class: Postgres truth + per-chat sequence_number. System
-- messages (sender NULL) are the platform posting to its own chat.
-- Forward-only; owned by sqlx-cli.

CREATE TYPE group_chat_kind AS ENUM ('dm', 'group', 'project');
CREATE TYPE group_member_role AS ENUM ('owner', 'admin', 'member');
CREATE TYPE group_msg_type AS ENUM ('user', 'system');

CREATE TABLE group_chats (
    id         UUID            PRIMARY KEY,
    kind       group_chat_kind NOT NULL,
    name       TEXT,
    project_id UUID            REFERENCES projects(id) ON DELETE CASCADE,  -- set for project chats
    created_by UUID            REFERENCES users(id),
    created_at TIMESTAMPTZ     NOT NULL DEFAULT now()
);
-- One project chat per project.
CREATE UNIQUE INDEX group_chats_project_idx ON group_chats (project_id) WHERE kind = 'project';

CREATE TABLE group_chat_members (
    group_chat_id UUID              NOT NULL REFERENCES group_chats(id) ON DELETE CASCADE,
    user_id       UUID              NOT NULL REFERENCES users(id),
    role          group_member_role NOT NULL DEFAULT 'member',
    added_at      TIMESTAMPTZ       NOT NULL DEFAULT now(),
    PRIMARY KEY (group_chat_id, user_id)
);
CREATE INDEX group_chat_members_user_idx ON group_chat_members (user_id);

CREATE TABLE group_chat_messages (
    id               UUID           PRIMARY KEY,
    group_chat_id    UUID           NOT NULL REFERENCES group_chats(id) ON DELETE CASCADE,
    sender_user_id   UUID           REFERENCES users(id),          -- NULL for system messages
    message_type     group_msg_type NOT NULL DEFAULT 'user',
    sequence_number  INT            NOT NULL,                      -- monotonic per chat
    content          TEXT           NOT NULL,
    attachments      JSONB,
    shared_resources JSONB,
    mentions         JSONB,
    created_at       TIMESTAMPTZ    NOT NULL DEFAULT now(),
    edited_at        TIMESTAMPTZ,
    UNIQUE (group_chat_id, sequence_number)
);
CREATE INDEX group_chat_messages_seq_idx ON group_chat_messages (group_chat_id, sequence_number);

CREATE TABLE group_chat_notes (
    id            UUID        PRIMARY KEY,
    group_chat_id UUID        NOT NULL REFERENCES group_chats(id) ON DELETE CASCADE,
    content       TEXT        NOT NULL,
    version       INT         NOT NULL DEFAULT 1,   -- optimistic concurrency token
    updated_by    UUID        REFERENCES users(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX group_chat_notes_chat_idx ON group_chat_notes (group_chat_id);
