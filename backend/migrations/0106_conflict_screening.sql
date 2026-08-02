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

-- Checking a caller against the practice's own records.
--
-- A practice has to know whether the person on the telephone is already somewhere in its
-- records before it offers them anything, and until now a line could not know. This is
-- the list it checks against and the answer it reached.
--
-- Deliberately a table and not a Library. Anything attached to the answering agent as a
-- Library is retrieved into the conversation, which would put the most confidential list
-- a practice owns in front of an anonymous caller; and retrieval returns prose rather
-- than a yes or a no, so it could not answer the question even if that were safe.
-- Comparing a name against a list is a matter of reducing both sides to one form and
-- looking, which is what the application does on the way in and out of this table.
--
-- One list, and it does not record which side of anything anybody is on. The question a
-- receptionist can answer is "does this name appear in our records", and the answer to
-- yes is "a person needs to deal with this". Sorting callers into clients and opponents
-- is a professional judgement, and it is not one to make by telephone.

CREATE TABLE conflict_names (
    id            UUID PRIMARY KEY,
    -- Whose records these are. The practice is an account here, as it is everywhere else
    -- in this feature: the same account a line runs as.
    owner_user_id UUID NOT NULL REFERENCES users(id),
    -- As somebody wrote it, so a person reading the list recognises it.
    name          TEXT NOT NULL,
    -- The same name reduced to the single form both sides of a check are compared in,
    -- written by the application. Stored rather than computed per check so that the
    -- comparison is an index lookup and so that a change to the reduction rules is a
    -- visible, deliberate rewrite rather than a silent change of meaning.
    normalised    TEXT NOT NULL,
    -- Which matter, or whatever else the person adding it wanted recorded. This is what
    -- lets somebody resolve a match quickly instead of guessing why the line refused.
    note          TEXT,
    created_by    UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One entry per name per practice, whatever spelling it arrived in. A list is pasted in
-- bulk and pasted again later, so the same name arriving twice must be one row.
CREATE UNIQUE INDEX conflict_names_key ON conflict_names (owner_user_id, normalised);
-- The list as a person reads it, and the count shown beside a line.
CREATE INDEX conflict_names_owner_idx ON conflict_names (owner_user_id, name);

-- What the check concluded about this call.
--
-- Null means nobody checked, and that is treated exactly as a match is: a call may only
-- be handed to a person when it has been checked and found clear. So forgetting to check
-- refuses the transfer rather than allowing it, which is the only way round a check like
-- this can safely be.
ALTER TABLE calls
    ADD COLUMN conflict_check TEXT
        CHECK (conflict_check IS NULL OR conflict_check IN ('clear', 'possible', 'unknown'));
