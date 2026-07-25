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

-- An automation may run against a connected folder on a paired machine instead
-- of on the server. When it does, the schedule still lives here (the server is
-- the source of truth for when it fires), but the folder work is carried out by
-- the machine's own hands, addressed over its live connection.
--
-- Three new outcomes a run can have:
--   'missed'        the machine was offline, or a run of the same automation was
--                   still going, when this occurrence came due: recorded, not
--                   silently dropped, so the owner can see it happened;
--   'needs_approval' a state-changing step is waiting for the owner to agree,
--                   with the worker freed rather than held;
--   'superseded'    a missed run that a reconnect catch-up has taken over, so a
--                   double reconnect cannot enqueue the same make-up run twice.
--
-- New enum labels cannot be used in the same transaction that adds them, so no
-- statement below writes one of these values; the runtime does.
ALTER TYPE automation_run_status ADD VALUE IF NOT EXISTS 'missed';
ALTER TYPE automation_run_status ADD VALUE IF NOT EXISTS 'needs_approval';
ALTER TYPE automation_run_status ADD VALUE IF NOT EXISTS 'superseded';

-- The folder an automation runs against. NULL keeps today's behaviour exactly: a
-- server automation. The machine is not stored separately: it is whichever
-- machine owns this folder, so the two can never disagree, and withdrawing the
-- folder (SET NULL) quietly returns the automation to running on the server.
ALTER TABLE automations
    ADD COLUMN workspace_id        UUID    NULL REFERENCES device_workspaces(id) ON DELETE SET NULL,
    -- The owner's explicit, per-automation agreement that this job may write
    -- files in its folder without asking on every occurrence. Deletion is never
    -- covered by it. Only meaningful with a folder, so only settable with one.
    ADD COLUMN pre_approved_writes BOOLEAN NOT NULL DEFAULT FALSE,
    -- When the machine was offline at firing time, make the missed occurrence up
    -- once the machine comes back (within a window), rather than waiting for the
    -- next scheduled slot.
    ADD COLUMN run_when_back        BOOLEAN NOT NULL DEFAULT FALSE;

-- Why an occurrence was missed ('offline' | 'overlap' | 'workspace withdrawn').
-- A failure still explains itself in `error`; this is only for the misses, which
-- are not failures.
ALTER TABLE automation_runs ADD COLUMN reason TEXT NULL;

-- The reconnect catch-up looks up, per machine, its automations' most recent
-- runs by status; this keeps that lookup off a sequential scan.
CREATE INDEX automation_runs_status_started_idx
    ON automation_runs (automation_id, status, started_at DESC);
