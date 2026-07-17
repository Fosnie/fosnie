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

//! Chain verification — recompute every row and confirm the links hold.
//!
//! Generic over the executor so a caller can verify inside an uncommitted
//! transaction (used by the tamper tests, which mutate then roll back).
//!
//! Offline evidence-pack attestation lives in the sibling [`super::verify_export`]
//! module (part of Fosnie Enterprise); this file is the Core tamper-detection half.

use super::{compute_chain_hash, AuditOutcome};

/// Result of verifying the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainStatus {
    pub ok: bool,
    pub checked: u64,
    /// `seq` of the first row that failed, if any.
    pub first_bad_seq: Option<i64>,
    pub reason: Option<String>,
}

impl ChainStatus {
    fn good(checked: u64) -> Self {
        Self {
            ok: true,
            checked,
            first_bad_seq: None,
            reason: None,
        }
    }
    fn bad(seq: i64, reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            checked: 0,
            first_bad_seq: Some(seq),
            reason: Some(reason.into()),
        }
    }
}

/// Walk the chain in `seq` order; recompute each hash and confirm each row's
/// `prev_hash` links to its predecessor.
pub async fn verify_chain<'e, E>(executor: E) -> Result<ChainStatus, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let rows = sqlx::query!(
        r#"
        SELECT seq,
               actor_user_id,
               actor_role,
               action_type,
               resource_type,
               resource_id,
               occurred_at,
               session_id,
               source_ip,
               outcome AS "outcome: AuditOutcome",
               outcome_reason,
               model_agent_traceability,
               token_usage,
               risk_anomaly_flag,
               payload,
               prev_hash,
               hash,
               signature
        FROM audit_events
        ORDER BY seq ASC
        "#
    )
    .fetch_all(executor)
    .await?;

    // Verifying key, if audit signing is configured in this process.
    let verifying = super::signing().map(|k| k.verifying_key());

    let mut expected_prev: Option<Vec<u8>> = None;
    let mut checked: u64 = 0;
    let mut first = true;

    for row in rows {
        // Linkage: the stored prev_hash must match the predecessor's hash.
        // The first PRESENT row anchors the chain — its predecessor may have
        // been dropped by a retention partition-drop, so we
        // accept its stored prev_hash rather than requiring it to resolve.
        if first {
            expected_prev = row.prev_hash.clone();
            first = false;
        } else if row.prev_hash != expected_prev {
            return Ok(ChainStatus::bad(row.seq, "prev_hash does not link to predecessor"));
        }

        // Integrity: recompute the hash from the row's content + prev.
        let recomputed = compute_chain_hash(
            row.seq,
            row.actor_user_id,
            &row.actor_role,
            &row.action_type,
            row.resource_type.as_deref(),
            row.resource_id,
            row.occurred_at,
            row.session_id.as_deref(),
            row.source_ip.as_deref(),
            row.outcome,
            row.outcome_reason.as_deref(),
            row.model_agent_traceability.as_ref(),
            row.token_usage.as_ref(),
            row.risk_anomaly_flag,
            row.payload.as_ref(),
            expected_prev.as_deref(),
        );

        if recomputed != row.hash {
            return Ok(ChainStatus::bad(row.seq, "row hash does not match recomputed content"));
        }

        // Signature: when signing is configured and the row carries one, it must
        // verify against the row hash. Unsigned legacy rows are skipped (the
        // hash-chain remains the primary guarantee).
        if let (Some(vk), Some(sig_bytes)) = (verifying.as_ref(), row.signature.as_ref()) {
            let ok = ed25519_dalek::Signature::from_slice(sig_bytes)
                .map(|sig| ed25519_dalek::Verifier::verify(vk, &row.hash, &sig).is_ok())
                .unwrap_or(false);
            if !ok {
                return Ok(ChainStatus::bad(row.seq, "Ed25519 signature does not verify"));
            }
        }

        expected_prev = Some(row.hash);
        checked += 1;
    }

    Ok(ChainStatus::good(checked))
}
