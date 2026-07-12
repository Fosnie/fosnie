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

-- 0013_feedback.sql — Message-level feedback on assistant answers.
-- One row per (message, user); thumbs up/down + optional
-- comment, tied to the Agent + model context. Local only. Forward-only; sqlx-cli.

CREATE TYPE feedback_rating AS ENUM ('up', 'down');

CREATE TABLE feedback (
    id         UUID            PRIMARY KEY,
    message_id UUID            NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id    UUID            NOT NULL REFERENCES users(id),
    rating     feedback_rating NOT NULL,
    comment    TEXT,
    agent_id   UUID            REFERENCES agents(id),   -- context: which Agent produced the answer
    model      TEXT,                                    -- context: which model (best-effort)
    created_at TIMESTAMPTZ     NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ     NOT NULL DEFAULT now(),
    UNIQUE (message_id, user_id)                        -- one rating per user per message (upsert)
);
CREATE INDEX feedback_agent_idx ON feedback (agent_id);
