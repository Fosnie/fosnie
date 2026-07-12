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

-- 0091_multi_llm_provider.sql — several named LLM providers per scope + a
-- per-conversation active pointer (phase 1: llm role only).
--
-- Today `provider_configs` holds exactly one row per (role, scope, scope_id),
-- enforced by two partial unique indexes. This migration lets the `llm` role hold
-- MANY named rows at each scope (a "Claude", a "GPT", a "Local vLLM"), while every
-- other role stays single-row exactly as before. Each chat remembers which llm
-- provider it uses (`chats.llm_provider_id`); a stale/deleted pointer SET NULLs and
-- falls back to the deployment default → the ML .env default. Forward-only.
--
--   * `label`      — human display name; meaningful for llm, ignored by single roles.
--   * `is_default` — marks the deployment fallback llm used when a chat has no pick.
--
-- Zero behaviour change on upgrade: the single existing llm row is backfilled to
-- label='Default', is_default=true; existing chats keep llm_provider_id = NULL and
-- resolve to that default (i.e. today's provider).

ALTER TABLE provider_configs ADD COLUMN label      TEXT;
ALTER TABLE provider_configs ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT false;

-- Restrict the existing single-row uniqueness to NON-llm roles, so llm can hold
-- several rows per scope while embed/rerank/ocr/stt/tts/verify stay one-per-scope.
DROP INDEX provider_configs_deployment_uniq;
DROP INDEX provider_configs_user_uniq;
CREATE UNIQUE INDEX provider_configs_deployment_uniq
    ON provider_configs (role) WHERE scope = 'deployment' AND role <> 'llm';
CREATE UNIQUE INDEX provider_configs_user_uniq
    ON provider_configs (role, scope_id) WHERE scope = 'user' AND role <> 'llm';

-- No two llm rows share a label within a scope. COALESCE the NULL deployment
-- scope_id to a fixed sentinel so deployment rows collide on label as intended
-- (Postgres treats NULLs as distinct in a plain unique index).
CREATE UNIQUE INDEX provider_configs_llm_label_uniq
    ON provider_configs (scope, COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), label)
    WHERE role = 'llm';

-- At most one default llm per scope (deployment has one; a user's own set has one).
CREATE UNIQUE INDEX provider_configs_llm_default_uniq
    ON provider_configs (scope, COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE role = 'llm' AND is_default;

-- Per-conversation active provider. ON DELETE SET NULL so deleting/disabling the
-- chosen provider never errors a turn — resolution falls back to the deployment
-- default, then the ML .env default.
ALTER TABLE chats
    ADD COLUMN llm_provider_id UUID REFERENCES provider_configs(id) ON DELETE SET NULL;

-- Backfill: name any existing llm row and mark the deployment one the default, so an
-- upgraded single-provider deploy is byte-identical (one named default, NULL chat
-- pointers → same resolution as before).
UPDATE provider_configs
   SET label = COALESCE(NULLIF(model, ''), 'Default')
 WHERE role = 'llm' AND label IS NULL;
-- Mark every pre-existing llm row (one per scope) its scope's default, so an
-- upgraded deploy AND any pre-existing per-user BYOK llm row keep being resolved
-- exactly as before (deployment default for normal users; the user's own row
-- still wins for a BYOK user who had one). The per-scope default index permits one
-- default per (scope, scope_id), and only one row exists per scope pre-migration.
UPDATE provider_configs
   SET is_default = true
 WHERE role = 'llm';
