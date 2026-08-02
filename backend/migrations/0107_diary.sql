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

-- A diary, so a caller can be offered a time.
--
-- One diary per account, because a practice has one set of opening hours and one stream
-- of appointments, and because a receptionist asked to work out which of several diaries
-- a caller wants is a receptionist with a new way to be wrong.
--
-- Two decisions here are load-bearing and neither is obvious.
--
-- **The diary keeps a real time zone.** Everything else scheduled in this platform is
-- resolved against UTC, which is why a job asked to run at nine in the morning runs at
-- ten for half the year. A diary cannot inherit that: a caller offered an appointment an
-- hour out, twice a year, in the direction nobody notices until they arrive, is worse
-- than a diary that does not exist. Opening hours below are minutes from LOCAL midnight,
-- and turning those into instants is the application's job, against the zone named here.
--
-- **Appointments sit on a fixed grid.** Their length comes from the diary rather than
-- from each booking, so two appointments either start at the same instant or do not
-- overlap at all. That is what lets a unique index stop two callers taking one slot.
-- Appointments of arbitrary length would need an exclusion constraint over a range type,
-- which needs an extension, and no migration here has ever required one: the schema runs
-- on stock Postgres, including the managed kind where creating an extension is refused.

CREATE TABLE diaries (
    owner_user_id UUID PRIMARY KEY REFERENCES users(id),
    -- A zone name from the standard database, checked by the application against the
    -- zone data itself. Not a fixed offset: an offset cannot know when the clocks change.
    timezone      TEXT NOT NULL,
    -- How long an appointment is, and therefore how the grid is spaced.
    slot_minutes  INT  NOT NULL DEFAULT 30  CHECK (slot_minutes BETWEEN 5 AND 240),
    -- How soon from now something may be booked, so a caller cannot take a slot that
    -- starts before anybody could have read about it.
    lead_minutes  INT  NOT NULL DEFAULT 120 CHECK (lead_minutes BETWEEN 0 AND 20160),
    -- And how far ahead, so nothing is offered next spring.
    horizon_days  INT  NOT NULL DEFAULT 30  CHECK (horizon_days BETWEEN 1 AND 180),
    -- Off until somebody has looked at the opening hours, like a line.
    enabled       BOOL NOT NULL DEFAULT false,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- When the practice is open, in minutes from local midnight.
--
-- Several rows per weekday rather than one pair of times, so a lunch break is two rows
-- instead of a special case, and so a practice that opens twice on a Saturday can say so.
-- Minutes rather than a time-of-day type because the arithmetic that builds the grid is
-- addition, and because no other column in this schema uses one.
CREATE TABLE diary_hours (
    owner_user_id UUID     NOT NULL REFERENCES diaries(owner_user_id) ON DELETE CASCADE,
    -- 0 is Monday, matching the way a week is written down here.
    weekday       SMALLINT NOT NULL CHECK (weekday BETWEEN 0 AND 6),
    opens_minute  INT      NOT NULL CHECK (opens_minute BETWEEN 0 AND 1440),
    closes_minute INT      NOT NULL CHECK (closes_minute BETWEEN 0 AND 1440),
    CHECK (closes_minute > opens_minute),
    PRIMARY KEY (owner_user_id, weekday, opens_minute)
);

-- Days the practice is shut, whatever the opening hours say.
--
-- A local calendar date rather than an instant, because a holiday is a day in the
-- practice's own reckoning and not a period of twenty-four hours from midnight anywhere
-- else.
CREATE TABLE diary_closures (
    owner_user_id UUID NOT NULL REFERENCES diaries(owner_user_id) ON DELETE CASCADE,
    closed_on     DATE NOT NULL,
    note          TEXT,
    PRIMARY KEY (owner_user_id, closed_on)
);

-- One appointment.
CREATE TABLE appointments (
    id            UUID PRIMARY KEY,
    owner_user_id UUID NOT NULL REFERENCES users(id),
    starts_at     TIMESTAMPTZ NOT NULL,
    ends_at       TIMESTAMPTZ NOT NULL,
    status        TEXT NOT NULL DEFAULT 'booked' CHECK (status IN ('booked', 'cancelled')),
    -- What the caller was told to quote if they ring back. Half of how somebody is
    -- identified later, so it is short enough to say aloud and unique while it is live.
    reference     TEXT NOT NULL,
    -- All of these as the caller gave them, aloud, and therefore all unverified.
    caller_name   TEXT NOT NULL,
    caller_e164   TEXT NOT NULL DEFAULT '',
    contact       TEXT,
    subject       TEXT NOT NULL,
    -- The call and the conversation it was arranged on, when it was arranged by
    -- telephone. Both clear themselves rather than taking the appointment with them: a
    -- booking outlives the record of the call that made it.
    call_id       UUID REFERENCES calls(id) ON DELETE SET NULL,
    chat_id       UUID REFERENCES chats(id) ON DELETE SET NULL,
    -- Who booked it from the interface, when a person did rather than a caller.
    created_by    UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    cancelled_at  TIMESTAMPTZ
);

-- The whole of how two callers cannot take one slot, and the reason appointments are a
-- fixed length. Partial, so cancelling gives the time back rather than blocking it for
-- ever: the losing insert is told nought rows, which is what the agent turns into "that
-- has just gone".
CREATE UNIQUE INDEX appointments_slot_key
    ON appointments (owner_user_id, starts_at) WHERE status = 'booked';
-- One live appointment per reference per practice, because a reference that matched two
-- would be a reference that identifies nobody.
CREATE UNIQUE INDEX appointments_reference_key
    ON appointments (owner_user_id, reference) WHERE status = 'booked';
-- The diary as somebody reads it, and the lookup that decides a slot is free.
CREATE INDEX appointments_diary_idx ON appointments (owner_user_id, starts_at DESC);

-- How many times this call has tried to name an appointment it did not book.
--
-- A caller who cannot be identified must not be able to keep guessing: six characters of
-- reference is sixteen million combinations, which is far beyond reach at three tries a
-- call and well within it given unlimited ones.
ALTER TABLE calls ADD COLUMN appointment_attempts SMALLINT NOT NULL DEFAULT 0;
