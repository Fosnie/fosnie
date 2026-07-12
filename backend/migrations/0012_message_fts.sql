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

-- 0012_message_fts.sql — Full-text search over team-chat messages
-- (cross-message search). Replaces the ILIKE scan with a
-- maintained tsvector + GIN index (stemming + ranking). Generated column, so no
-- trigger to maintain. Forward-only; owned by sqlx-cli.

ALTER TABLE group_chat_messages
    ADD COLUMN content_tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('english', content)) STORED;

CREATE INDEX group_chat_messages_fts_idx ON group_chat_messages USING GIN (content_tsv);
