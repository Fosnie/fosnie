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

-- Putting a caller through to a person.
--
-- Until now a call could only end: the caller hung up, the network said it was over, or
-- the line went quiet long enough to be given up on. An agent that could not help had
-- nothing to offer but a message. This is the other answer, and the record of it.
--
-- Nothing here reaches out to the telephone network. The line the caller is on already
-- belongs to a conversation the network is holding open, and when we stop speaking it
-- asks us what to do next. So a transfer is a thing we are asked about rather than a
-- thing we go and do, and the columns below are what that answer is read from.

-- A call that left us for somebody else.
--
-- Its own outcome rather than a kind of completion: "the caller got through to a person"
-- and "the caller was finished with" are the two things anybody looking at a line wants
-- told apart, and a log that called them both completed could not answer either.
ALTER TABLE calls DROP CONSTRAINT calls_outcome_check;
ALTER TABLE calls
    ADD CONSTRAINT calls_outcome_check
        CHECK (outcome IN ('in_progress', 'completed', 'carrier_ended', 'dropped',
                           'no_media', 'line_full', 'transferred'));

-- The number the network was asked to ring, written before anything is closed.
--
-- Written down rather than remembered, because what is asked next arrives as a fresh
-- request that knows only which call it is about: it may land after this process has
-- forgotten the call, or after a restart, and either way the answer has to be the same.
ALTER TABLE calls
    ADD COLUMN transfer_to TEXT
        CHECK (transfer_to IS NULL OR transfer_to ~ '^\+[1-9][0-9]{6,14}$');

-- Where this line puts callers through to, and the whole of what makes it possible: no
-- number, no transfer, and the agent is not offered the ability in the first place.
--
-- One number, set by an administrator, and never something the agent chooses. If where
-- to ring were the agent's decision it would be a caller's decision, and a caller who
-- can pick the number is a caller who can have this deployment ring anybody, from its
-- own line, at its own expense.
ALTER TABLE phone_numbers
    ADD COLUMN transfer_e164 TEXT
        CHECK (transfer_e164 IS NULL OR transfer_e164 ~ '^\+[1-9][0-9]{6,14}$');

-- A handover is the third thing a caller leaves behind.
--
-- Whoever picks the telephone up gets a ringing telephone and nothing else, so what the
-- caller had already explained would be lost between one and the other, and they would
-- be asked it all again. It is a record in the same sense as the other two and is read
-- in the same place, under the same rule about who may read it.
ALTER TABLE enquiries DROP CONSTRAINT enquiries_kind_check;
ALTER TABLE enquiries
    ADD CONSTRAINT enquiries_kind_check
        CHECK (kind IN ('message', 'lead', 'handover'));
