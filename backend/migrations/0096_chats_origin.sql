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

-- Which client created a conversation.
--
-- The three values are declared together on purpose, even though only two can
-- occur today: 'web' (the application), 'api' (a programmatic completion) and
-- 'desktop' (a desktop client, once one exists). Declaring the third now means
-- no later migration and, more importantly, fixes the visibility rule before
-- anyone can get it wrong.
--
-- That rule is EXCLUDE 'api', never "only web". A conversation held from
-- another first-class client belongs in the user's history alongside the rest,
-- marked by where it came from rather than hidden. Only completions driven by
-- an external application are kept out of the chat lists, because they are
-- machine traffic and would drown the sidebar. They stay openable by direct URL
-- for debugging, and readable through the ordinary chat endpoints.
--
-- Existing rows are all application conversations, so the default backfills
-- them correctly and no data migration is needed.
--
-- Note on project exports: those select by project, and an API conversation has
-- no project, so no filter is needed there. Adding one would imply a guarantee
-- that path does not rely on.
ALTER TABLE chats
    ADD COLUMN origin TEXT NOT NULL DEFAULT 'web'
        CHECK (origin IN ('web', 'api', 'desktop'));

-- The sidebar reads one owner's live conversations excluding machine traffic.
CREATE INDEX chats_owner_origin_idx ON chats (owner_user_id, created_at DESC)
    WHERE archived_at IS NULL AND origin <> 'api';
