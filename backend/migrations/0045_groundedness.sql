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

-- 0045_groundedness.sql — Groundedness verification, Slice 1: Mode A (live
-- RAG-answer faithfulness). Every
-- factual claim in an output is checked against the sources the system holds and
-- unsupported spans are flagged — groundedness, not truth. `verification_runs` is
-- one verification of one target (a message in live mode; a document/draft in the
-- later Mode B); `claim_verdicts` is the per-claim/per-span result. The compact
-- summary is ALSO denormalised onto `messages.groundedness` (like `activity` in
-- 0044) so chat history renders the score pill + flagged spans in one query.
-- Full §6 shape is laid now; live mode populates the subset it produces. Each run
-- is a first-class hash-chain audit event (`groundedness.verified`). Forward-only.

ALTER TABLE messages ADD COLUMN groundedness JSONB;  -- {score,total,flagged,model,spans:[{start,end,text}]}

CREATE TYPE verification_target AS ENUM ('message', 'document', 'draft');
CREATE TYPE verification_mode   AS ENUM ('live', 'verify_draft');
CREATE TYPE claim_verdict       AS ENUM ('supported', 'contradicted', 'not_mentioned');

-- One verification run over one target. Aggregate score + counts; the verifier
-- model + strictness are recorded so a result is reproducible/auditable.
CREATE TABLE verification_runs (
    id                 UUID                PRIMARY KEY,
    target_type        verification_target NOT NULL,
    target_id          UUID                NOT NULL,            -- message/document/draft id (no FK: polymorphic)
    mode               verification_mode   NOT NULL,
    verifier_model     TEXT                NOT NULL,
    strictness         TEXT                NOT NULL DEFAULT 'strict',
    faithfulness_score DOUBLE PRECISION,                        -- grounded fraction ∈ [0,1]; NULL = verifier was down
    total_claims       INT                 NOT NULL DEFAULT 0,
    supported          INT                 NOT NULL DEFAULT 0,
    contradicted       INT                 NOT NULL DEFAULT 0,
    not_mentioned      INT                 NOT NULL DEFAULT 0,
    status             TEXT                NOT NULL DEFAULT 'succeeded',
    created_by         UUID                REFERENCES users(id),
    created_at         TIMESTAMPTZ         NOT NULL DEFAULT now(),
    finished_at        TIMESTAMPTZ
);
CREATE INDEX verification_runs_target_idx ON verification_runs (target_type, target_id);

-- Per-claim (Mode B) / per-flagged-span (Mode A live) verdict. In live mode each
-- row is an unsupported span (verdict = not_mentioned) with its char offsets.
CREATE TABLE claim_verdicts (
    id                 UUID          PRIMARY KEY,
    run_id             UUID          NOT NULL REFERENCES verification_runs(id) ON DELETE CASCADE,
    claim_text         TEXT          NOT NULL,
    source_span        JSONB,                                  -- {start,end} char offsets in the output
    bound_evidence_ref JSONB,                                  -- {doc_id/chunk_id/citation_id}; NULL when un-cited
    had_citation       BOOL          NOT NULL DEFAULT false,
    verdict            claim_verdict NOT NULL,
    verifier_score     DOUBLE PRECISION,
    repair_action      TEXT,                                   -- 'regenerated'|'cut'|'kept'; NULL in live mode
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT now()
);
CREATE INDEX claim_verdicts_run_idx ON claim_verdicts (run_id);
