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

-- What a caller is told, and how long what they said is kept.
--
-- Until now a line answered in silence: the caller spoke first and was recognised,
-- screened and possibly booked without ever being told they were speaking to a machine
-- or that their words were written down. And nothing a call produced was ever deleted.
-- Both of those are settings a practice has to be able to hold, so both are columns.
--
-- No new tables, which is the point of doing it this way: everything here belongs to a
-- line or to a call, and erasure already takes both.

-- The notice this line reads out, for the lines whose practice needs its own wording.
-- NULL means the built-in notice, which is where every line starts and where most
-- should stay: the standard wording is the one that has been thought about.
ALTER TABLE phone_numbers ADD COLUMN notice TEXT;

-- Retention, per line, both dormant at nought.
--
-- A deployment deletes nothing until somebody decides it should, because the opposite
-- default would quietly destroy a practice's record of who rang them. Two periods
-- rather than one: the words a caller said and the fact that they rang are different
-- things to a practice, and are often kept for different lengths of time.
ALTER TABLE phone_numbers ADD COLUMN transcript_days INT NOT NULL DEFAULT 0
    CHECK (transcript_days BETWEEN 0 AND 3650);
ALTER TABLE phone_numbers ADD COLUMN log_days INT NOT NULL DEFAULT 0
    CHECK (log_days BETWEEN 0 AND 3650);

-- What this caller was actually told, and when they were told it.
--
-- The words themselves rather than a flag saying they were said. A line's notice can be
-- edited afterwards, and the question a complaint asks is what was said on that call,
-- not what the line happens to say today.
ALTER TABLE calls ADD COLUMN notice_at   TIMESTAMPTZ;
ALTER TABLE calls ADD COLUMN notice_text TEXT;

-- The conversation has been deleted, by the sweep or by hand.
--
-- Kept as a fact about the call, so a log entry with no conversation reads as tidied
-- away rather than as one that never had anything to say.
ALTER TABLE calls ADD COLUMN transcript_deleted_at TIMESTAMPTZ;

-- A call that was answered but never got its notice out, and so was ended rather than
-- carried. Nobody is listened to who has not been told what they are speaking to, and
-- this is that decision as a recorded outcome rather than a silent hang-up.
ALTER TABLE calls DROP CONSTRAINT calls_outcome_check;
ALTER TABLE calls ADD CONSTRAINT calls_outcome_check CHECK (outcome IN
    ('in_progress', 'completed', 'carrier_ended', 'dropped', 'no_media', 'line_full',
     'transferred', 'notice_failed'));

-- What the sweep walks: finished calls on one line, oldest first. Partial, because a
-- call still in progress is never a candidate for deletion and there is no sense in
-- carrying it in the index that exists to find deletable ones.
CREATE INDEX calls_retention_idx ON calls (phone_number_id, ended_at) WHERE ended_at IS NOT NULL;
