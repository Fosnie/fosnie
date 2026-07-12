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

-- 0081_abac_policies.sql — attribute-based access policies expressed in Cedar
-- (D4). Each row is one Cedar policy (permit/forbid) validated against the Cedar
-- schema before it is stored; a `forbid` is the ONLY mechanism that can take
-- access away (it overrides flat grants and roles alike — but never break-glass).
--
--   * policy_text      — the Cedar source; must parse + type-check against the
--     versioned schema (backend rejects a policy that does not).
--   * enabled          — enabling is a separate, deliberate action; a stored
--     policy does nothing until switched on. Only enabled policies compile into
--     the in-memory PolicySet.
--   * last_validated_at — when the text last passed schema validation.
--
-- The engine (compile enabled policies → PolicySet, per-request entity slice)
-- lands with the Enterprise edition; this table is empty and inert in Core.

CREATE TABLE abac_policies (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name              TEXT        NOT NULL UNIQUE,
    description       TEXT        NOT NULL DEFAULT '',
    policy_text       TEXT        NOT NULL,
    enabled           BOOLEAN     NOT NULL DEFAULT false,
    created_by        UUID        REFERENCES users(id),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_validated_at TIMESTAMPTZ
);

-- Hot path at startup + on invalidation: load the enabled policies to compile.
CREATE INDEX abac_policies_enabled_idx ON abac_policies (enabled) WHERE enabled;
