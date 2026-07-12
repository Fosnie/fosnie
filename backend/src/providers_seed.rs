// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `LOCAL_STACK` provider seed — the "fully-local" deploy (docker-compose
//! `--profile local`, which sets `LOCAL_STACK=1`) wires the three inference roles
//! at the bundled container endpoints so chat + RAG work with zero manual config.
//!
//! Idempotent and non-destructive: each role is inserted only when no
//! deployment-scope row exists yet (`ON CONFLICT … DO NOTHING`), so an admin who
//! later points a role elsewhere is never overwritten, and a re-`up` seeds nothing
//! new. `api_key` is left NULL — Ollama and the local reranker need none.

use crate::error::Result;
use crate::state::AppState;

struct LocalSeed {
    role: &'static str,
    base_url: &'static str,
    model: &'static str,
}

/// LLM + embeddings served by the Ollama container (OpenAI-shape `/v1`), rerank by
/// the llama.cpp container. Model tags match `install.sh`'s `ollama pull`s.
const LOCAL_SEEDS: [LocalSeed; 3] = [
    LocalSeed { role: "llm", base_url: "http://ollama:11434/v1", model: "qwen3:4b" },
    LocalSeed {
        role: "embed",
        base_url: "http://ollama:11434/v1",
        model: "hf.co/ggml-org/bge-m3-Q8_0-GGUF:Q8_0",
    },
    LocalSeed { role: "rerank", base_url: "http://reranker:8091", model: "Qwen3-Reranker-0.6B" },
];

/// True when the deployment asked for the bundled local inference stack.
pub fn enabled() -> bool {
    std::env::var("LOCAL_STACK")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Seed the local-stack provider rows (idempotent). Best-effort caller: a failure
/// must not block boot.
pub async fn seed_local_stack(state: &AppState) -> Result<()> {
    for s in LOCAL_SEEDS.iter() {
        let mut tx = state.pg.begin().await?;
        // Migration 0091 (multi-LLM) split the deployment uniqueness: `llm` may hold
        // many rows per scope (unique on the label index), while every other role
        // stays one-per-scope on `(role) WHERE scope='deployment' AND role<>'llm'`.
        // A single `ON CONFLICT (role)` arbiter therefore no longer matches any index —
        // seed each role against the index that actually governs it.
        let rows_affected = if s.role == "llm" {
            // Give the local llm a stable label so re-`up` collides (idempotent), and
            // make it the deployment default only when none exists yet — guards the
            // separate one-default-per-scope index (`provider_configs_llm_default_uniq`).
            sqlx::query!(
                r#"INSERT INTO provider_configs
                       (id, role, scope, scope_id, base_url, model, label, is_default,
                        api_key_encrypted, enabled, updated_by, updated_at)
                   VALUES ($1, 'llm', 'deployment', NULL, $2, $3, 'Local (Ollama)',
                        NOT EXISTS (SELECT 1 FROM provider_configs
                                    WHERE role = 'llm' AND scope = 'deployment' AND is_default),
                        NULL, true, NULL, now())
                   ON CONFLICT (scope, COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), label)
                       WHERE role = 'llm'
                   DO NOTHING"#,
                uuid::Uuid::now_v7(),
                s.base_url,
                s.model,
            )
            .execute(&mut *tx)
            .await?
            .rows_affected()
        } else {
            sqlx::query!(
                r#"INSERT INTO provider_configs
                       (id, role, scope, scope_id, base_url, model, api_key_encrypted, enabled, updated_by, updated_at)
                   VALUES ($1, $2, 'deployment', NULL, $3, $4, NULL, true, NULL, now())
                   ON CONFLICT (role) WHERE scope = 'deployment' AND role <> 'llm'
                   DO NOTHING"#,
                uuid::Uuid::now_v7(),
                s.role,
                s.base_url,
                s.model,
            )
            .execute(&mut *tx)
            .await?
            .rows_affected()
        };

        if rows_affected > 0 {
            let mut event = crate::audit::AuditEvent::action("provider.seeded", "system");
            event.resource_type = Some("provider_config".into());
            event.payload = Some(serde_json::json!({
                "role": s.role,
                "scope": "deployment",
                "base_url": s.base_url,
                "model": s.model,
                "source": "LOCAL_STACK",
            }));
            crate::audit::append_with(&mut tx, &event).await?;
            tracing::info!(role = s.role, base_url = s.base_url, "LOCAL_STACK: seeded local provider");
        }
        tx.commit().await?;
    }
    Ok(())
}
