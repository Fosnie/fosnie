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

-- 0019_artefact_message.sql — link a generated artefact to the assistant message
-- (and turn) that produced it, so the UI can render it inline under that answer
-- (not as a chat-wide floating panel). Both nullable: an artefact is created
-- mid-turn (turn_id known) and linked to its message once the turn persists.
-- Forward-only; owned by sqlx-cli.

ALTER TABLE generated_artefacts
    ADD COLUMN turn_id    UUID,
    ADD COLUMN message_id UUID REFERENCES messages(id) ON DELETE SET NULL;

CREATE INDEX generated_artefacts_message_idx ON generated_artefacts (message_id);
