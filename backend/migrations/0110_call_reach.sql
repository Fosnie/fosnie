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

-- Reaching the systems a practice actually runs on, from a telephone call.
--
-- Two halves of one problem. A tool that changes something outside this deployment is
-- held for a person to approve, which is right everywhere except on a telephone call:
-- there the caller is listening, nobody is watching an approval, and the wait is
-- minutes of silence on a live line. And nothing this deployment learns has ever been
-- able to leave it, so a message taken at four in the afternoon is seen when somebody
-- next opens the app rather than when it is taken.

-- Whether this server's side-effecting tools may be used during a telephone call.
--
-- Refused by default, and per server rather than deployment-wide, because the caller
-- is an anonymous member of the public and what they can reach is whatever the line's
-- agent holds. Marking a server is an operator saying "this one, on a call, knowingly".
ALTER TABLE mcp_servers ADD COLUMN call_policy TEXT NOT NULL DEFAULT 'refuse'
    CHECK (call_policy IN ('refuse', 'allow'));

-- The same decision for a tool this deployment defined itself.
ALTER TABLE custom_tools ADD COLUMN allow_on_call BOOL NOT NULL DEFAULT false;

-- Where an account wants to be told about what its lines took.
--
-- Several are allowed on purpose: a practice may want appointments in one channel and
-- messages in another, and the people who need to see each are rarely the same.
CREATE TABLE notify_targets (
    id            UUID PRIMARY KEY,
    owner_user_id UUID NOT NULL REFERENCES users(id),
    label         TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('slack', 'teams', 'webhook')),
    -- Held encrypted, because an incoming-webhook address is a bearer credential:
    -- anybody who has it can post into that channel as though they were the practice.
    -- It is never sent back out of this deployment, only the host it points at.
    url_enc       TEXT NOT NULL,
    -- Which events this target takes. Empty means none, which is what a target created
    -- without a choice does: silence is the safe way round when the alternative is
    -- announcing a caller's name into a channel nobody meant to.
    events        TEXT[] NOT NULL DEFAULT '{}',
    enabled       BOOL NOT NULL DEFAULT true,
    created_by    UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The whole of what delivery looks up: this account's live targets. Partial, because a
-- switched-off target is never a candidate and there is no sense carrying it here.
CREATE INDEX notify_targets_owner_idx ON notify_targets (owner_user_id) WHERE enabled;
