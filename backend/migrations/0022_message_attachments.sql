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

-- 0022_message_attachments.sql — persisted file/image attachments for group and
-- direct (DM) chat messages. The group_chat_messages.attachments JSONB already
-- exists and stores a list of `{ id, filename, mime }`; this table is the backing
-- store (bytes on disk, pointer + metadata here). Download access is gated by
-- membership of a group chat whose message references the attachment, or being
-- the uploader (see http/message_attachments.rs). Forward-only; owned by sqlx-cli.

CREATE TABLE message_attachments (
    id          UUID        PRIMARY KEY,
    uploaded_by UUID        REFERENCES users(id),
    filename    TEXT        NOT NULL,
    mime        TEXT        NOT NULL,
    byte_size   BIGINT      NOT NULL,
    disk_path   TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
