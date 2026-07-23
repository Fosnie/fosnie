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

-- A folder on a paired machine that its owner has connected to their account.
--
-- The folder's contents are never here. What is here is the fact that a
-- particular machine was told a particular path may be worked in, at what level
-- of trust, and when that was agreed: enough to show the user what they have
-- granted, to withdraw it, and to answer afterwards for what was done. The path
-- is a string for display and for the record, not a handle: every actual
-- boundary check happens against the real filesystem, on the machine that has
-- one.
--
-- Trust is one of three levels, and it only ever narrows what may happen:
--   'ro'    read only;
--   'rw'    read, and write or delete;
--   'rw_nd' read and write, but never delete.
CREATE TABLE device_workspaces (
    id           UUID        PRIMARY KEY,
    device_id    UUID        NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    path         TEXT        NOT NULL,
    label        TEXT        NOT NULL DEFAULT '',
    tier         TEXT        NOT NULL CHECK (tier IN ('ro', 'rw', 'rw_nd')),
    trusted_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NULL,
    revoked_at   TIMESTAMPTZ NULL
);

CREATE INDEX device_workspaces_device_idx ON device_workspaces (device_id, trusted_at DESC);
CREATE INDEX device_workspaces_user_idx ON device_workspaces (user_id, trusted_at DESC);

-- Connecting the same folder twice on one machine returns the grant already
-- held rather than making a second one, so withdrawing it withdraws it. A
-- machine may connect a folder again after withdrawing it, which is why revoked
-- rows are exempt.
CREATE UNIQUE INDEX device_workspaces_path_uniq ON device_workspaces (device_id, path)
    WHERE revoked_at IS NULL;

-- The folder a conversation is working in. One at a time and one only: a turn
-- that could reach two folders would have to explain, in every prompt and every
-- approval, which one it meant.
CREATE TABLE chat_workspace (
    chat_id      UUID        PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    workspace_id UUID        NOT NULL REFERENCES device_workspaces(id) ON DELETE CASCADE,
    set_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chat_workspace_workspace_idx ON chat_workspace (workspace_id);

-- Commands the owner has already agreed to for one folder, matched by the start
-- of the command line. Agreeing to `npm test` once and being asked again every
-- time teaches people to stop reading the question, so the second identical run
-- goes ahead: still recorded, just not re-asked.
--
-- Deletion is deliberately not expressible here. It is the one action whose cost
-- cannot be undone by running it again more carefully, so it is asked every
-- time, and the code that consults this table refuses to consider it at all.
CREATE TABLE workspace_command_prefixes (
    id           UUID        PRIMARY KEY,
    workspace_id UUID        NOT NULL REFERENCES device_workspaces(id) ON DELETE CASCADE,
    prefix       TEXT        NOT NULL,
    added_by     UUID        NULL REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX workspace_command_prefixes_uniq
    ON workspace_command_prefixes (workspace_id, prefix);
