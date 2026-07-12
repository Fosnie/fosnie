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

-- 0021_chat_shares.sql — sharing an LLM chat into a team/group/DM chat. A chat is
-- otherwise readable only by its owner (or project members if it carries a
-- project). A share records that chat C was posted into group chat G; read access
-- then extends to G's members (see http/export.rs require_chat_read), so the
-- "open the shared chat" link works for everyone in that group/DM. Used both by
-- the user "Share" action and by automation delivery (the output chat link).
-- Forward-only; owned by sqlx-cli.

CREATE TABLE chat_shares (
    chat_id       UUID        NOT NULL REFERENCES chats(id)        ON DELETE CASCADE,
    group_chat_id UUID        NOT NULL REFERENCES group_chats(id)  ON DELETE CASCADE,
    shared_by     UUID        REFERENCES users(id),
    shared_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, group_chat_id)
);
CREATE INDEX chat_shares_chat_idx ON chat_shares (chat_id);
