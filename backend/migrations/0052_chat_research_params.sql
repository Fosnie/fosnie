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

-- 0052_chat_research_params.sql — Refine support (cancel-and-refine). A Deep Research run's request parameters
-- (question, source, template, depth, kb_ids, refinements) are stashed on its
-- research chat so the DR home can re-open prefilled ('Refine' = a fresh run
-- with the same scope). NULL on every non-research chat; additive, forward-only.

ALTER TABLE chats ADD COLUMN research_params JSONB;
