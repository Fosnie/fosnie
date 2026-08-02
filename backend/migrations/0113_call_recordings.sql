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

-- Keeping the audio of a call, so somebody can hear what was said.
--
-- Until now nothing here kept a second of sound: speech was recognised as it arrived
-- and the samples were discarded, which is why the notice a caller hears says their
-- words are written down rather than that the call is recorded. Both of those change
-- together on a line that records, and they change together on purpose: a recording
-- nobody was told about is the one version of this feature not worth having.

-- Whether this line records the conversation, and for how long the audio is kept.
--
-- Off, and no period, is where every line starts and where it stays until somebody
-- decides otherwise.
ALTER TABLE phone_numbers ADD COLUMN record_calls BOOL NOT NULL DEFAULT false;
ALTER TABLE phone_numbers ADD COLUMN recording_days INT NOT NULL DEFAULT 0
    CHECK (recording_days BETWEEN 0 AND 3650);

-- The period is not optional once recording is on.
--
-- Every other retention period here treats nought as "keep indefinitely", which is the
-- right default for a line of text and the wrong one for a voice recording: it is the
-- bulkiest and most sensitive thing this feature produces, and one nobody set a period
-- for is one kept for ever by accident. So the two settings are one decision, and the
-- column is where that is made unrepresentable rather than merely discouraged.
ALTER TABLE phone_numbers ADD CONSTRAINT phone_numbers_recording_period
    CHECK (NOT record_calls OR recording_days > 0);

-- Where this call's audio is, and what it is.
--
-- The path is a suffix relative to the recordings directory, like every other file this
-- product writes, so moving an installation does not orphan them. Nullable throughout:
-- most calls have no recording, and a call whose audio has been deleted keeps its record
-- and loses only the sound.
ALTER TABLE calls ADD COLUMN recording_path    TEXT;
ALTER TABLE calls ADD COLUMN recording_bytes   BIGINT;
ALTER TABLE calls ADD COLUMN recording_seconds INT;

-- The line was set to record and no audio came of it: a disk that filled, a write that
-- failed. Kept as a fact about the call, because "there is no recording" and "the
-- recording failed" are different answers to somebody asking to hear one.
ALTER TABLE calls ADD COLUMN recording_failed  BOOL NOT NULL DEFAULT false;

-- What the sweep walks: calls that have audio, oldest first. Partial, because a call
-- without a recording is never a candidate for having one deleted.
CREATE INDEX calls_recording_idx ON calls (phone_number_id, ended_at)
    WHERE recording_path IS NOT NULL;
