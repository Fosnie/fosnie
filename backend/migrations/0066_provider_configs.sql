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

-- 0066_provider_configs.sql — runtime provider selection.
--
-- Lifts LLM/embed/rerank/ocr/stt/tts/verify provider config out of the ML
-- service's .env into the DB so an admin can point a role at a local or external
-- API (Claude/GPT/Gemini/…) at runtime. An empty table = ML keeps its .env
-- defaults (behaviour-identical). API keys are stored AES-256-GCM encrypted
-- (crypto.rs); only ciphertext lives here.
--
-- Scope-aware from day one: `scope='deployment'` (scope_id NULL) is all 4a writes;
-- `scope='user'` (scope_id = user id) is reserved for per-user BYOK (4b). Forward-only.

CREATE TABLE provider_configs (
    id                UUID        PRIMARY KEY,
    role              TEXT        NOT NULL,   -- llm|embed|rerank|ocr|stt|tts|verify
    scope             TEXT        NOT NULL,   -- 'deployment' | 'user'
    scope_id          UUID,                   -- NULL for deployment; user_id for user
    base_url          TEXT,
    model             TEXT,
    api_key_encrypted TEXT,                   -- crypto.rs AES-256-GCM ciphertext
    enabled           BOOLEAN     NOT NULL DEFAULT true,
    updated_by        UUID        REFERENCES users(id),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One row per role at deployment scope. A plain UNIQUE(role, scope, scope_id)
-- would NOT dedupe deployment rows (Postgres treats NULL scope_id as distinct),
-- so enforce uniqueness with partial indexes per scope.
CREATE UNIQUE INDEX provider_configs_deployment_uniq
    ON provider_configs (role) WHERE scope = 'deployment';
CREATE UNIQUE INDEX provider_configs_user_uniq
    ON provider_configs (role, scope_id) WHERE scope = 'user';
