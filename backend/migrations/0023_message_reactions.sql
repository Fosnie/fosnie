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

-- 0023_message_reactions.sql — emoji reactions on group/DM messages. One row per
-- (message, user, emoji); toggling adds/removes a row. (Note: this intentionally
-- extends beyond REQUIREMENTS §4.5 "no reactions" — added at product request.)
-- Forward-only; owned by sqlx-cli.

CREATE TABLE message_reactions (
    message_id UUID        NOT NULL REFERENCES group_chat_messages(id) ON DELETE CASCADE,
    user_id    UUID        NOT NULL REFERENCES users(id),
    emoji      TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, user_id, emoji)
);
CREATE INDEX message_reactions_message_idx ON message_reactions (message_id);
