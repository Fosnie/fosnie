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

-- Telephone lines, and a log of the calls they took.
--
-- Until now a deployment answered one number, described by three deployment-wide
-- settings. A line is now a row: its own number, its own agent, and its own owning
-- account, so one instance can answer for several people or teams and the boundary
-- between them is a row rather than a comment.
--
-- All of it in one migration because the parts are not independent. A number that
-- can be bound but whose conversations cannot be stamped, or a call log with no
-- lines to point at, is a half-built feature that a half-applied deployment would
-- present as a whole one.

-- A conversation held aloud, over a telephone line.
--
-- The rule declared with this column was EXCLUDE 'api', never "only web", so that a
-- new first-class client would need no change to any reader. This is that promise
-- being collected: a call belongs in its owner's history alongside everything else,
-- marked by where it came from rather than hidden, and only machine traffic driven
-- by an external application stays out of the lists.
ALTER TABLE chats DROP CONSTRAINT chats_origin_check;
ALTER TABLE chats
    ADD CONSTRAINT chats_origin_check
        CHECK (origin IN ('web', 'api', 'desktop', 'phone'));

-- A telephone line: a number, the agent that answers it, and the account it runs as.
--
-- The owner and the agent are both required, which is the point of the table rather
-- than a detail of it. A caller has no account and never signs in, so what they can
-- reach is exactly what that agent can reach, running as that account. An unbound
-- line would be a public telephone number attached to nothing in particular, so it
-- is made unrepresentable here instead of refused later.
CREATE TABLE phone_numbers (
    id            UUID PRIMARY KEY,
    -- Full international form, and only that. The pattern is enforced here as well
    -- as on write because the answer path compares the number it was called on
    -- against this column with no transformation at all: exact equality is what
    -- makes it impossible for one incoming call to match two lines.
    e164          TEXT NOT NULL CHECK (e164 ~ '^\+[1-9][0-9]{6,14}$'),
    provider      TEXT NOT NULL DEFAULT 'twilio' CHECK (provider IN ('twilio')),
    owner_user_id UUID NOT NULL REFERENCES users(id),
    agent_id      UUID NOT NULL REFERENCES agents(id),
    -- What to call this line in a list. A page of bare numbers is unreadable once
    -- there are more than about three.
    label         TEXT,
    -- What the line should say when it answers. Stored and edited now; speaking it
    -- is the work that comes with consent and recording notices, since the first
    -- thing a caller must be told is what they are speaking to.
    greeting      TEXT,
    -- A new line is created switched off, so it cannot start answering in the
    -- seconds between being created and being checked.
    enabled       BOOL NOT NULL DEFAULT false,
    created_by    UUID REFERENCES users(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The answer path's entire lookup, and the guarantee that one number answers once.
CREATE UNIQUE INDEX phone_numbers_e164_key ON phone_numbers (e164);

-- One answered call.
--
-- Only calls that were answered are here. A refused call was never picked up and so
-- was never a call: those are in the audit trail with the reason they were refused,
-- which is a different question ("why is my line not answering?") from the one this
-- table answers ("what happened on my line?").
CREATE TABLE calls (
    id               UUID PRIMARY KEY,
    -- The line it came in on, kept nullable with the number denormalised beside it:
    -- a log that disappears when a number is released is not a log.
    phone_number_id  UUID REFERENCES phone_numbers(id) ON DELETE SET NULL,
    provider         TEXT NOT NULL,
    provider_call_id TEXT NOT NULL,
    to_e164          TEXT NOT NULL,
    -- Empty means the caller withheld their number.
    from_e164        TEXT NOT NULL DEFAULT '',
    -- The binding as it was when the call ran, not as it is now: rebinding a line
    -- must not rewrite what already happened on it. The agent is nullable because
    -- erasing an account deletes the agents it created, and history has to survive
    -- that even though the agent does not.
    owner_user_id    UUID NOT NULL REFERENCES users(id),
    agent_id         UUID REFERENCES agents(id) ON DELETE SET NULL,
    -- The conversation, once there is one. A caller who says nothing produces no
    -- conversation, and no empty one is created to fill this in.
    chat_id          UUID REFERENCES chats(id) ON DELETE SET NULL,
    outcome          TEXT NOT NULL DEFAULT 'in_progress'
        CHECK (outcome IN ('in_progress', 'completed', 'carrier_ended', 'dropped',
                           'no_media', 'line_full')),
    started_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at         TIMESTAMPTZ
);

-- One row per call however many times the carrier retries telling us about it.
CREATE UNIQUE INDEX calls_provider_call_idx ON calls (provider, provider_call_id);
-- The log's default order, and the cursor it pages by.
CREATE INDEX calls_recent_idx ON calls (started_at DESC, id DESC);
-- One line's history.
CREATE INDEX calls_line_idx ON calls (phone_number_id, started_at DESC);
-- Calls still open. Read at startup to close the ones a stopped process left
-- behind; partial so that sweep never has to walk the whole history.
CREATE INDEX calls_open_idx ON calls (started_at) WHERE ended_at IS NULL;
