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

-- 0020_automation_targets.sql — per-automation targeting.
-- An automation was a bare prompt→agent run with no Project, no Library, and no
-- delivery target; its output chat landed with project_id = NULL (hidden outside
-- General workmode). Add three optional targets:
--   * project_id            — the run's output chat inherits this Project (sector).
--   * kb_ids                — Libraries attached at run time (materialised as
--                             chat_kb_links on the created chat; the fail-closed
--                             intersection allow-list still governs access).
--   * deliver_group_chat_id — on success, post a system message into this internal
--                             group chat (Teams). In-platform, zero egress.
-- ON DELETE SET NULL so removing a project / group chat never blocks on a
-- referencing automation. Forward-only; owned by sqlx-cli.

ALTER TABLE automations
    ADD COLUMN project_id            UUID   REFERENCES projects(id)     ON DELETE SET NULL,
    ADD COLUMN kb_ids                UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN deliver_group_chat_id UUID   REFERENCES group_chats(id)  ON DELETE SET NULL;
