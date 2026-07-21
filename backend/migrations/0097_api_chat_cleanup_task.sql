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

-- Durable task kind for pruning aged programmatic conversations.
--
-- Alone in its own migration: a new enum value cannot be used inside the
-- transaction that adds it, so anything referencing it must land separately.
ALTER TYPE task_type ADD VALUE IF NOT EXISTS 'api_chat_cleanup';
