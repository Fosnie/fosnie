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

-- 0049_web_search_deep_task.sql — Add the `web_search_deep` durable-task kind so
-- a `depth=deep` web search
-- can run as a background job on the existing scheduler queue (retry/backoff/
-- dead-letter reused): the exhaustive, politely-paced loop runs unbounded by the
-- tool timeout and posts its digest + citations back into the chat when done.
-- Separate migration: ALTER TYPE … ADD VALUE must not be used in the same
-- transaction it is added in. Forward-only; owned by sqlx-cli.

ALTER TYPE task_type ADD VALUE IF NOT EXISTS 'web_search_deep';
