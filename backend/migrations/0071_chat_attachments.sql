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

-- 0071_chat_attachments.sql — durable backing store for per-turn chat (LLM) message
-- attachments, so a user's uploaded files render under their message and in the docs
-- rail (live + after reload). Bytes live on disk (storage.chat_attachments_dir);
-- this table holds the pointer + metadata. chat_id/message_id are NULL at upload
-- time (the chat may not exist yet on the first turn) and backfilled when the turn
-- persists the user message (see chat::run_turn). Download access is gated by being
-- the uploader or having chat-read on chat_id (see http/chat_attachments.rs). Orphan
-- rows (uploaded but never sent → message_id stays NULL) are pruned by a periodic
-- task. Forward-only; owned by sqlx-cli.

CREATE TABLE chat_attachments (
    id            UUID        PRIMARY KEY,
    chat_id       UUID        REFERENCES chats(id) ON DELETE CASCADE,
    message_id    UUID        REFERENCES messages(id) ON DELETE CASCADE,
    owner_user_id UUID        REFERENCES users(id),
    filename      TEXT        NOT NULL,
    mime          TEXT        NOT NULL,
    byte_size     BIGINT      NOT NULL,
    disk_path     TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chat_attachments_message_idx ON chat_attachments (message_id);
CREATE INDEX chat_attachments_orphan_idx ON chat_attachments (created_at) WHERE message_id IS NULL;
