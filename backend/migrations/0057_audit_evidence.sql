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
-- FEATURE A2 — audit / compliance evidence export.
--
-- Three tables that turn the existing hash-chain audit into portable, tamper-
-- evident, crypto-shreddable regulatory evidence:
--
--  * subject_keys        — per-data-subject AES-256 key (wrapped by the deployment
--                          master key). GDPR erasure = DELETE the row (crypto-shred):
--                          all that subject's evidence ciphertext becomes permanently
--                          undecryptable while the hash chain still verifies. This is
--                          the ONLY mutable/deletable table of the three.
--  * interaction_evidence— rich per-interaction provenance. PII-bearing fields are
--                          encrypted with the subject key; only the row's content_hash
--                          is chained into audit_events (payload.evidence_content_hash),
--                          so the immutable chain never holds plaintext PII.
--  * audit_checkpoints   — periodic Ed25519-signed chain heads (seq + head hash + ts):
--                          the externally-attestable, zero-egress substitute for a
--                          blockchain. Append-only (see 0058).

-- ---------------------------------------------------------------------------
-- subject_keys — per-subject key material; the crypto-shred lever.
-- ---------------------------------------------------------------------------
CREATE TABLE subject_keys (
    subject_id   UUID         PRIMARY KEY,         -- the data subject (a user id)
    wrapped_key  TEXT         NOT NULL,            -- subject AES key, wrapped by the master key
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    shredded_at  TIMESTAMPTZ                       -- set if ever re-created post-erasure
);

-- ---------------------------------------------------------------------------
-- interaction_evidence — one row per assistant interaction (+ agentic sub-steps
-- link via trace_id). PII fields are ciphertext; non-PII provenance is plaintext.
-- No FK to messages/users: evidence must outlive its source for compliance.
-- ---------------------------------------------------------------------------
CREATE TABLE interaction_evidence (
    interaction_id     UUID         PRIMARY KEY,   -- = the assistant message id
    trace_id           UUID,                       -- groups sub-steps (chat/turn/run)
    subject_id         UUID,                        -- → subject_keys.subject_id (the asker)
    keycloak_subject   TEXT,
    -- PII-bearing, per-subject encrypted (base64(nonce‖ct) or 'pt:'-prefixed in dev):
    prompt_ct          TEXT,
    output_ct          TEXT,
    subqueries_ct      TEXT,
    retrieval_ct       TEXT,                        -- retrieved snippet text
    -- Non-PII provenance (plaintext, queryable):
    model_name         TEXT,
    model_revision     TEXT,
    vllm_version       TEXT,
    sampling           JSONB,                       -- {temperature, top_p, max_tokens, seed}
    tool_registry_version TEXT,
    retrieval_meta     JSONB,                       -- [{doc_id, doc_version, chunk_id, scores, round}] — NO snippet text
    guardrail_actions  JSONB,
    redaction_actions  JSONB,                       -- {category, tier} only — never raw PII
    groundedness       JSONB,                       -- {score, method, version}
    citation_coverage  REAL,
    abstention         BOOLEAN      NOT NULL DEFAULT false,
    finish_reason      TEXT,
    prompt_tokens      INT,
    completion_tokens  INT,
    total_tokens       INT,
    latency_ms         INT,
    human_action       JSONB,                       -- {action, reviewer, reviewed_at, reason}
    content_hash       BYTEA        NOT NULL,        -- SHA256(domain ‖ canonical(all fields))
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX interaction_evidence_subject_idx ON interaction_evidence (subject_id);
CREATE INDEX interaction_evidence_trace_idx   ON interaction_evidence (trace_id);
CREATE INDEX interaction_evidence_created_idx ON interaction_evidence (created_at);

-- ---------------------------------------------------------------------------
-- audit_checkpoints — Ed25519-signed chain heads (the credibility multiplier).
-- ---------------------------------------------------------------------------
CREATE TABLE audit_checkpoints (
    id                UUID         PRIMARY KEY,
    seq_no            BIGINT       NOT NULL,         -- chain head seq at checkpoint time
    head_hash         BYTEA        NOT NULL,         -- hash of the head row
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    signature         BYTEA,                         -- Ed25519 over (domain ‖ seq_no ‖ head_hash ‖ ts); NULL if signing unset
    public_key        TEXT,                          -- verifying key hex, for offline attestation
    client_cosignature BYTEA,                        -- optional out-of-band client co-sign
    cosigned_at       TIMESTAMPTZ
);

CREATE INDEX audit_checkpoints_seq_idx ON audit_checkpoints (seq_no DESC);
