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

-- 0047_repair_task.sql — Add the `repair_run` durable-task kind so ground-or-cut
-- repair (groundedness §4.6 / §5 step 5) of a finished verify-draft run can run as
-- a background job: regenerate or cut each flagged claim, re-verify the new
-- citation, and surface the result as tracked-change proposals on the document.
-- The repair text + new evidence + tracked-change `w_id` ride the existing
-- `claim_verdicts.bound_evidence_ref` JSONB and the existing `repair_action`
-- column, so no table change is needed — only the new enum value. Separate
-- migration: ALTER TYPE … ADD VALUE must not share its transaction. Forward-only.

ALTER TYPE task_type ADD VALUE IF NOT EXISTS 'repair_run';
