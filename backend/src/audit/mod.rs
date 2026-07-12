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

//! Audit log — append-only, hash-chained, tamper-evident.
//!
//! Each event carries a global monotonic `seq` and a `hash` =
//! `SHA256(domain ‖ canonical(fields) ‖ prev_hash)`, binding it to its
//! predecessor. Appends serialise on a Postgres advisory lock so the chain is
//! consistent under concurrency. [`verify::verify_chain`] recomputes the chain
//! to detect any tampering.
//!
//! **Separation of duties** (§A.2.2): the application role is granted only
//! INSERT/SELECT on `audit_events` (no UPDATE/DELETE) — the log is not
//! forgeable through the app even by an admin. Retention deletes by dropping
//! whole partitions on a privileged path, never row-by-row here.
//!
//! Deferred (Pass-2): optional Ed25519 `signature`, evidence-package export,
//! partition-rolling automation, the retention job itself.

pub mod verify;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Advisory-lock key dedicated to the audit chain (ASCII "PAIAUDIT").
const CHAIN_LOCK_KEY: i64 = 0x5041_4941_5544_4954u64 as i64;

/// Domain-separation tag — bump if the canonical serialisation ever changes.
const HASH_DOMAIN: &[u8] = b"pai.audit.chain.v1";

/// Optional Ed25519 signing key (audit §A.2.2). Set once at boot from
/// `BootConfig.audit_signing_key`; absent = unsigned (hash-chain only).
static SIGNING_KEY: std::sync::OnceLock<Option<ed25519_dalek::SigningKey>> =
    std::sync::OnceLock::new();

/// Initialise audit signing from a 32-byte hex seed (empty = disabled). Called
/// once at startup; idempotent (first set wins).
pub fn init_signing(seed_hex: &str) {
    init_signing_bytes(parse_seed_bytes(seed_hex));
}

/// Initialise audit signing from raw seed bytes (the BYOK path — the seed is
/// unwrapped by a [`crate::ext::KeyProvider`], e.g. from an HSM). `None` ⇒ unsigned.
/// Idempotent (first set wins).
pub fn init_signing_bytes(seed: Option<[u8; 32]>) {
    let _ = SIGNING_KEY.set(seed.map(|arr| ed25519_dalek::SigningKey::from_bytes(&arr)));
}

/// Parse a 32-byte hex audit seed (empty/invalid ⇒ `None`). Public so the Core
/// [`EnvFileKeyProvider`](crate::ext::EnvFileKeyProvider) can hand the raw seed to
/// [`init_signing_bytes`] through the KeyProvider seam.
pub fn parse_seed_bytes(seed_hex: &str) -> Option<[u8; 32]> {
    let s = seed_hex.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = hex::decode(s).ok()?;
    bytes.try_into().ok()
}

fn signing() -> Option<&'static ed25519_dalek::SigningKey> {
    SIGNING_KEY.get().and_then(|o| o.as_ref())
}

/// The registered audit sink (extension seam). Set once at boot, like the signing
/// key; unset ⇒ the Core default [`BasicAuditSink`] (hash-chain, no row signature).
/// A private `fosnie-enterprise` crate registers its tamper-evident `ChainAuditSink`
/// via [`init_sink`].
static AUDIT_SINK: std::sync::OnceLock<std::sync::Arc<dyn crate::ext::AuditSink>> =
    std::sync::OnceLock::new();

/// Register the audit sink. Called once at startup (idempotent; first set wins).
/// When never called, [`append`]/[`append_with`] use the Core [`BasicAuditSink`].
pub fn init_sink(sink: std::sync::Arc<dyn crate::ext::AuditSink>) {
    let _ = AUDIT_SINK.set(sink);
}

/// The active sink: the registered one, or the Core default ([`BasicAuditSink`]).
fn sink() -> std::sync::Arc<dyn crate::ext::AuditSink> {
    AUDIT_SINK
        .get()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(BasicAuditSink))
}

/// The audit public key (hex), if signing is configured — for evidence export.
pub fn public_key_hex() -> Option<String> {
    signing().map(|k| hex::encode(k.verifying_key().to_bytes()))
}

/// Ed25519-sign a message with the audit key, if signing is configured. Used for
/// checkpoint heads (A2); event-row signatures are produced inline in `append_with`.
pub fn sign(message: &[u8]) -> Option<Vec<u8>> {
    signing().map(|k| ed25519_dalek::Signer::sign(k, message).to_bytes().to_vec())
}

/// Verify an Ed25519 signature against a hex-encoded verifying key. Pure function
/// (no global key) so the offline `audit verify` path can attest a checkpoint from
/// the public key embedded in an exported pack.
pub fn verify_signature(public_key_hex: &str, message: &[u8], signature: &[u8]) -> bool {
    let Ok(pk_bytes) = hex::decode(public_key_hex.trim()) else { return false };
    let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else { return false };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr) else { return false };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(signature) else { return false };
    ed25519_dalek::Verifier::verify(&vk, message, &sig).is_ok()
}

/// Outcome of an audited action. Maps to the `audit_outcome` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "audit_outcome", rename_all = "lowercase")]
pub enum AuditOutcome {
    Success,
    Failure,
}

impl AuditOutcome {
    /// Stable wire form used in the hash and at the DB boundary.
    pub fn as_str(self) -> &'static str {
        match self {
            AuditOutcome::Success => "success",
            AuditOutcome::Failure => "failure",
        }
    }
}

/// Everything captured for one interaction's compliance evidence (FEATURE A2).
/// PII fields (`prompt`/`output`/`subqueries`/`retrieval`) are plaintext here and
/// encrypted on the way in by [`evidence::capture`]. This is the parameter of the
/// [`crate::ext::EvidenceSink`] seam, so it lives in Core (like [`AuditEvent`])
/// even though the capture logic in [`evidence`] lives in Enterprise.
#[derive(Default)]
pub struct EvidenceInput {
    pub interaction_id: Uuid,
    pub trace_id: Option<Uuid>,
    pub subject_id: Option<Uuid>,
    pub keycloak_subject: Option<String>,
    pub prompt: Option<String>,
    pub output: Option<String>,
    pub subqueries: Option<String>,
    pub retrieval: Option<String>,
    pub model_name: Option<String>,
    pub model_revision: Option<String>,
    pub vllm_version: Option<String>,
    pub sampling: Option<serde_json::Value>,
    pub tool_registry_version: Option<String>,
    pub retrieval_meta: Option<serde_json::Value>,
    pub guardrail_actions: Option<serde_json::Value>,
    pub redaction_actions: Option<serde_json::Value>,
    pub groundedness: Option<serde_json::Value>,
    pub citation_coverage: Option<f32>,
    pub abstention: bool,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub latency_ms: Option<i32>,
    pub human_action: Option<serde_json::Value>,
}

/// A security-relevant event to be appended to the chain. Mint via
/// [`AuditEvent::action`] then set the optional fields you need.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub actor_user_id: Option<Uuid>,
    pub actor_role: String,
    pub action_type: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub occurred_at: OffsetDateTime,
    pub session_id: Option<String>,
    pub source_ip: Option<String>,
    pub outcome: AuditOutcome,
    pub outcome_reason: Option<String>,
    pub model_agent_traceability: Option<serde_json::Value>,
    pub token_usage: Option<serde_json::Value>,
    pub risk_anomaly_flag: bool,
    pub payload: Option<serde_json::Value>,
}

impl AuditEvent {
    /// A successful action by `actor_role`, timestamped now. Fill the rest as needed.
    pub fn action(action_type: impl Into<String>, actor_role: impl Into<String>) -> Self {
        Self {
            actor_user_id: None,
            actor_role: actor_role.into(),
            action_type: action_type.into(),
            resource_type: None,
            resource_id: None,
            occurred_at: OffsetDateTime::now_utc(),
            session_id: None,
            source_ip: None,
            outcome: AuditOutcome::Success,
            outcome_reason: None,
            model_agent_traceability: None,
            token_usage: None,
            risk_anomaly_flag: false,
            payload: None,
        }
    }
}

/// What an [`append`] wrote — useful for callers and tests.
#[derive(Debug, Clone)]
pub struct AppendResult {
    pub seq: i64,
    pub id: Uuid,
    pub hash: Vec<u8>,
}

/// Spawn the single audit-writer task; returns the hot-path sender and the
/// task handle (awaited at shutdown so the backlog drains before the runtime
/// tears down — re-audit R4b). Hot-path callers `enqueue` and return
/// immediately; this task drains the queue and writes the chain **in order** —
/// a single consumer is the ordering guarantee for `seq`/`prev_hash`. The
/// synchronous [`append`]/[`append_with`] remain for events that must be
/// durable and atomic with their action (they take the advisory lock; this
/// task does too, so the two paths stay consistent — note that means a queued
/// event can take a HIGHER `seq` than a later synchronous append it caused;
/// `occurred_at` keeps true causal time). The channel closes once all senders
/// drop; the loop drains the backlog, then exits.
///
/// Events are drained in batches under ONE transaction (lock + prev-hash read
/// amortised across the batch — re-audit R12); a failing batch falls back to
/// per-event appends so one poison event cannot lose its siblings, and any
/// event that still fails is counted on `audit_writer_failed_total` (R4a).
pub fn spawn_writer(pool: PgPool) -> (mpsc::Sender<AuditEvent>, Option<tokio::task::JoinHandle<()>>) {
    let (tx, mut rx) = mpsc::channel::<AuditEvent>(1024);
    if tokio::runtime::Handle::try_current().is_ok() {
        let handle = tokio::spawn(async move {
            let mut buf: Vec<AuditEvent> = Vec::with_capacity(64);
            loop {
                let n = rx.recv_many(&mut buf, 64).await;
                if n == 0 {
                    break; // channel closed and fully drained
                }
                let batch_ok = match pool.begin().await {
                    Ok(mut tx) => {
                        let mut ok = true;
                        for event in &buf {
                            if let Err(e) = append_with(&mut tx, event).await {
                                tracing::warn!(error = %e, action = %event.action_type,
                                    "audit batch append failed; retrying batch per-event");
                                ok = false;
                                break;
                            }
                        }
                        ok && tx.commit().await.is_ok()
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "audit batch begin failed; retrying per-event");
                        false
                    }
                };
                if !batch_ok {
                    for event in &buf {
                        if let Err(e) = append(&pool, event).await {
                            metrics::counter!("audit_writer_failed_total").increment(1);
                            tracing::error!(error = %e, action = %event.action_type,
                                "audit writer append failed; event lost");
                        }
                    }
                }
                buf.clear();
            }
            tracing::info!("audit writer task stopped");
        });
        (tx, Some(handle))
    } else {
        // No runtime (e.g. a synchronous unit test constructing AppState): drop the
        // receiver so enqueues are no-ops rather than panicking on spawn.
        drop(rx);
        (tx, None)
    }
}

/// Enqueue a hot-path event for the writer task (optimisation audit, L6). Drops
/// with a metric if the queue is saturated rather than block the request path —
/// events that must never be lost use [`append`]/[`append_with`] synchronously.
pub fn enqueue(tx: &mpsc::Sender<AuditEvent>, event: AuditEvent) {
    if let Err(err) = tx.try_send(event) {
        metrics::counter!("audit_enqueue_dropped_total").increment(1);
        match err {
            mpsc::error::TrySendError::Full(ev) => {
                tracing::warn!(action = %ev.action_type, "audit queue full; event dropped")
            }
            mpsc::error::TrySendError::Closed(ev) => {
                tracing::warn!(action = %ev.action_type, "audit queue closed; event dropped")
            }
        }
    }
}

/// Append an event to the chain in its own serialised transaction.
pub async fn append(pool: &PgPool, event: &AuditEvent) -> Result<AppendResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = append_with(&mut tx, event).await?;
    tx.commit().await?;
    Ok(result)
}

/// Append an event using a caller-supplied connection/transaction, so the
/// write can be made atomic with the action it records (e.g. a config change).
/// The advisory lock is held for the enclosing transaction's lifetime. Dispatches
/// to the registered [`AuditSink`](crate::ext::AuditSink) (Core default
/// [`ChainAuditSink`]) — call-sites are unchanged.
pub async fn append_with(
    conn: &mut sqlx::PgConnection,
    event: &AuditEvent,
) -> Result<AppendResult, sqlx::Error> {
    sink().append_with(conn, event).await
}

/// The shared chain-append body: advisory-lock → prev_hash → `seq` →
/// [`compute_chain_hash`] → INSERT, with the optional Ed25519 row `signature`
/// supplied by the caller via `sign` (invoked with the freshly-computed row hash).
/// This is the tamper-DETECTION core (hash-chain + linkage, Core); the optional
/// row-signature is layered on by the caller's closure, so [`BasicAuditSink`] (no
/// signature) and [`ChainAuditSink`] (signed) share one INSERT and stay byte-identical
/// to the previous single-sink behaviour.
pub async fn append_chain_with<F>(
    conn: &mut sqlx::PgConnection,
    event: &AuditEvent,
    sign: F,
) -> Result<AppendResult, sqlx::Error>
where
    F: FnOnce(&[u8]) -> Option<Vec<u8>>,
{
    // Permanent latency metric (optimisation audit Probe 2, kept per decision
    // §5.3 — observability is a platform component): time the lock-hold +
    // round-trips per append. Now measures the writer task's batched appends
    // and the synchronous/atomic path alike.
    let probe_start = std::time::Instant::now();
    // Surface flagged events to monitoring (the flag is otherwise write-only).
    if event.risk_anomaly_flag {
        metrics::counter!("audit_anomaly_total", "action" => event.action_type.clone()).increment(1);
    }
    // Serialise all appenders so prev_hash/seq are read consistently.
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", CHAIN_LOCK_KEY)
        .execute(&mut *conn)
        .await?;

    let prev_hash: Option<Vec<u8>> =
        sqlx::query_scalar!(r#"SELECT hash FROM audit_events ORDER BY seq DESC LIMIT 1"#)
            .fetch_optional(&mut *conn)
            .await?;

    let seq: i64 = sqlx::query_scalar!(r#"SELECT nextval('audit_events_seq') AS "seq!""#)
        .fetch_one(&mut *conn)
        .await?;

    let id = Uuid::now_v7();
    let hash = compute_chain_hash(
        seq,
        event.actor_user_id,
        &event.actor_role,
        &event.action_type,
        event.resource_type.as_deref(),
        event.resource_id,
        event.occurred_at,
        event.session_id.as_deref(),
        event.source_ip.as_deref(),
        event.outcome,
        event.outcome_reason.as_deref(),
        event.model_agent_traceability.as_ref(),
        event.token_usage.as_ref(),
        event.risk_anomaly_flag,
        event.payload.as_ref(),
        prev_hash.as_deref(),
    );

    // Optional Ed25519 signature over the row hash (non-repudiation, §A.2.2),
    // supplied by the sink: `ChainAuditSink` signs, `BasicAuditSink` returns None.
    let signature: Option<Vec<u8>> = sign(&hash);

    sqlx::query!(
        r#"
        INSERT INTO audit_events
            (seq, id, actor_user_id, actor_role, action_type, resource_type, resource_id,
             occurred_at, session_id, source_ip, outcome, outcome_reason,
             model_agent_traceability, token_usage, risk_anomaly_flag, payload, prev_hash, hash, signature)
        VALUES
            ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
        "#,
        seq,
        id,
        event.actor_user_id,
        event.actor_role,
        event.action_type,
        event.resource_type.as_deref(),
        event.resource_id,
        event.occurred_at,
        event.session_id.as_deref(),
        event.source_ip.as_deref(),
        event.outcome as AuditOutcome,
        event.outcome_reason.as_deref(),
        event.model_agent_traceability.as_ref(),
        event.token_usage.as_ref(),
        event.risk_anomaly_flag,
        event.payload.as_ref(),
        prev_hash.as_deref(),
        hash.as_slice(),
        signature.as_deref(),
    )
    .execute(&mut *conn)
    .await?;

    metrics::histogram!("audit_append_seconds").record(probe_start.elapsed().as_secs_f64());
    Ok(AppendResult { seq, id, hash })
}

/// The Core-default-**candidate** audit sink: hash-chain tamper-DETECTION with the
/// row `signature` column left NULL (no Ed25519). The hash-chain + prev_hash linkage
/// are preserved, so [`verify::verify_chain`] still detects tampering. Becomes the
/// Core default at the physical split; the signed [`ChainAuditSink`] stays the active
/// default in the combined build (so behaviour is unchanged here).
pub struct BasicAuditSink;

#[async_trait::async_trait]
impl crate::ext::AuditSink for BasicAuditSink {
    async fn append_with(
        &self,
        conn: &mut sqlx::PgConnection,
        event: &AuditEvent,
    ) -> Result<AppendResult, sqlx::Error> {
        append_chain_with(conn, event, |_hash| None).await
    }
}

/// Canonical, deterministic hash of a row's content chained to `prev_hash`.
///
/// Fields are emitted in a fixed order, each tagged and length-prefixed so no
/// two distinct rows can collide on concatenation. JSON is serialised with
/// `serde_json` (BTreeMap-backed, hence key-sorted) for stability. Timestamps
/// use nanoseconds since the Unix epoch. Shared by [`append`] and verification
/// so both sides agree byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub fn compute_chain_hash(
    seq: i64,
    actor_user_id: Option<Uuid>,
    actor_role: &str,
    action_type: &str,
    resource_type: Option<&str>,
    resource_id: Option<Uuid>,
    occurred_at: OffsetDateTime,
    session_id: Option<&str>,
    source_ip: Option<&str>,
    outcome: AuditOutcome,
    outcome_reason: Option<&str>,
    model_agent_traceability: Option<&serde_json::Value>,
    token_usage: Option<&serde_json::Value>,
    risk_anomaly_flag: bool,
    payload: Option<&serde_json::Value>,
    prev_hash: Option<&[u8]>,
) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();

    // tag byte + 8-byte big-endian length + bytes
    fn put(buf: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        buf.push(tag);
        buf.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        buf.extend_from_slice(bytes);
    }
    fn put_opt(buf: &mut Vec<u8>, tag: u8, bytes: Option<&[u8]>) {
        match bytes {
            Some(b) => {
                buf.push(1);
                put(buf, tag, b);
            }
            None => buf.push(0),
        }
    }
    fn put_json(buf: &mut Vec<u8>, tag: u8, v: Option<&serde_json::Value>) {
        match v {
            Some(v) => {
                buf.push(1);
                let bytes = serde_json::to_vec(v).expect("json value re-serialises");
                put(buf, tag, &bytes);
            }
            None => buf.push(0),
        }
    }

    put(&mut buf, 0x01, &seq.to_be_bytes());
    put_opt(
        &mut buf,
        0x02,
        actor_user_id.as_ref().map(|u| u.as_bytes().as_slice()),
    );
    put(&mut buf, 0x03, actor_role.as_bytes());
    put(&mut buf, 0x04, action_type.as_bytes());
    put_opt(&mut buf, 0x05, resource_type.map(str::as_bytes));
    put_opt(
        &mut buf,
        0x06,
        resource_id.as_ref().map(|u| u.as_bytes().as_slice()),
    );
    // Microseconds, not nanoseconds: Postgres `timestamptz` truncates to
    // microsecond precision, so hashing micros keeps append and verify in
    // agreement across the DB round-trip.
    let occurred_micros: i128 = occurred_at.unix_timestamp_nanos() / 1000;
    put(&mut buf, 0x07, &occurred_micros.to_be_bytes());
    put_opt(&mut buf, 0x08, session_id.map(str::as_bytes));
    put_opt(&mut buf, 0x09, source_ip.map(str::as_bytes));
    put(&mut buf, 0x0A, outcome.as_str().as_bytes());
    put_opt(&mut buf, 0x0B, outcome_reason.map(str::as_bytes));
    put_json(&mut buf, 0x0C, model_agent_traceability);
    put_json(&mut buf, 0x0D, token_usage);
    put(&mut buf, 0x0E, &[risk_anomaly_flag as u8]);
    put_json(&mut buf, 0x0F, payload);

    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(&buf);
    hasher.update(prev_hash.unwrap_or(&[]));
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod sign_tests {
    use super::*;

    #[test]
    fn ed25519_seed_signs_and_verifies() {
        let seed = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let bytes = parse_seed_bytes(seed).expect("valid seed");
        let key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let msg = b"audit-row-hash";
        let sig = ed25519_dalek::Signer::sign(&key, msg);
        let vk = key.verifying_key();
        assert!(ed25519_dalek::Verifier::verify(&vk, msg, &sig).is_ok());
        // A tampered message must not verify.
        assert!(ed25519_dalek::Verifier::verify(&vk, b"tampered", &sig).is_err());
        // Empty / malformed seeds disable signing.
        assert!(parse_seed_bytes("").is_none());
        assert!(parse_seed_bytes("zz").is_none());
    }
}
