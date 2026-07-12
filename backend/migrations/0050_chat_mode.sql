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

-- 0050_chat_mode.sql — Workspace-mode discriminator on chats.
-- Deep Research runs are chats under the
-- hood, listed ONLY in the Research mode and hidden from the General/Legal
-- lists. Existing chats (and every WS-created chat) default to 'general'; the
-- sector-based General/Legal scoping in the SPA is unchanged.

ALTER TABLE chats
    ADD COLUMN mode TEXT NOT NULL DEFAULT 'general'
    CHECK (mode IN ('general', 'legal', 'research'));

-- The Research-mode run list: an owner's research chats, newest first.
CREATE INDEX chats_research_idx ON chats (owner_user_id, created_at DESC)
    WHERE mode = 'research';
