// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Shared health/probe contracts in an owned DB. Synthetic keys, no network calls.
use fosnie_backend::{
    AppState,
    auth::AuthContext,
    model_routing::{failure::ProviderFailure, fallback::FailureKind, health, *},
};
use std::time::Duration;
use uuid::Uuid;
pub async fn exercise(state: &AppState, admin: &AuthContext) -> anyhow::Result<()> {
    let rows:Vec<(Uuid,Uuid,i64,i64,String)>=sqlx::query_as("SELECT p.id,p.connection_id,p.routing_revision,m.revision,p.model FROM provider_configs p JOIN model_profiles m ON m.provider_id=p.id WHERE p.scope='deployment' AND p.role='llm' AND p.connection_id IS NOT NULL ORDER BY p.connection_id,p.id").fetch_all(&state.pg).await?;
    let a = &rows[0];
    let b = rows.iter().find(|r| r.1 == a.1 && r.0 != a.0).unwrap();
    let target = |(id, _, rev, profile, model): &(Uuid, Uuid, i64, i64, String)| InferenceTarget {
        schema_version: 1,
        provider_id: *id,
        provider_revision: *rev,
        profile_revision: *profile,
        adapter: AdapterKind::OpenaiChat,
        base_url: "https://synthetic.invalid/v1".into(),
        model: model.clone(),
        api_key: "synthetic-key".into(),
        effort: None,
        generation: None,
        variant: None,
    };
    let mut ta = target(a);
    let tb = target(b);
    sqlx::query("DELETE FROM routing_health_scopes")
        .execute(&state.pg)
        .await?;
    sqlx::query("DELETE FROM routing_provider_health")
        .execute(&state.pg)
        .await?;
    let f = |kind| ProviderFailure {
        schema_version: 1,
        kind,
        before_output: true,
        retry_after_seconds: Some(30),
    };
    health::note(
        state,
        &ta,
        CallPurpose::Primary,
        &f(FailureKind::ModelUnavailable),
    )
    .await?;
    assert!(
        health::blocked(state, &ta.reference(), CallPurpose::Primary)
            .await?
            .is_some()
    );
    assert!(
        health::blocked(state, &tb.reference(), CallPurpose::Primary)
            .await?
            .is_none(),
        "404 model error cannot poison its connection peers"
    );
    sqlx::query("DELETE FROM routing_health_scopes")
        .execute(&state.pg)
        .await?;
    sqlx::query("DELETE FROM routing_provider_health")
        .execute(&state.pg)
        .await?;
    health::note(
        state,
        &ta,
        CallPurpose::Primary,
        &f(FailureKind::Unauthorized),
    )
    .await?;
    assert!(
        health::blocked(state, &tb.reference(), CallPurpose::Primary)
            .await?
            .is_some(),
        "connection auth outage shared without decrypting another model"
    );
    sqlx::query("UPDATE routing_health_scopes SET expires_at=now()-interval '1 second',probe_until=NULL,probe_token=NULL").execute(&state.pg).await?;
    sqlx::query("UPDATE routing_provider_health SET expires_at=now()-interval '1 second'")
        .execute(&state.pg)
        .await?;
    sqlx::query("UPDATE routing_credential_health SET expires_at=now()-interval '1 second'")
        .execute(&state.pg)
        .await?;
    let (pa, pb) = tokio::join!(
        health::recovery_probe(state, &ta, Duration::from_secs(2)),
        health::recovery_probe(state, &tb, Duration::from_secs(2))
    );
    assert_ne!(
        pa.is_ok(),
        pb.is_ok(),
        "exactly one worker probes a recovered connection"
    );
    let mut probe = pa.ok().or(pb.ok()).unwrap();
    probe.sent();
    probe.complete().await?;
    assert!(
        health::blocked(state, &tb.reference(), CallPurpose::Primary)
            .await?
            .is_none()
    );
    let c = rows.iter().find(|r| r.1 != a.1).unwrap();
    let group = Uuid::now_v7();
    sqlx::query("UPDATE provider_configs SET quota_group_id=$2 WHERE id=ANY($1)")
        .bind(vec![a.1, c.1])
        .bind(group)
        .execute(&state.pg)
        .await?;
    let mut tc = target(c);
    tc.api_key = "another-synthetic-key".into();
    ta.provider_revision =
        sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1")
            .bind(ta.provider_id)
            .fetch_one(&state.pg)
            .await?;
    tc.provider_revision =
        sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1")
            .bind(tc.provider_id)
            .fetch_one(&state.pg)
            .await?;
    health::register(state, &ta, CallPurpose::Primary).await?;
    health::register(state, &tc, CallPurpose::Primary).await?;
    health::note(
        state,
        &ta,
        CallPurpose::Primary,
        &f(FailureKind::QuotaExhausted),
    )
    .await?;
    assert!(
        health::blocked(state, &tc.reference(), CallPurpose::Primary)
            .await?
            .is_some(),
        "declared account quota shared across keys/connections"
    );
    let d = rows.iter().find(|r| r.1 != a.1 && r.1 != c.1).unwrap();
    let other_group = Uuid::now_v7();
    sqlx::query("UPDATE provider_configs SET quota_group_id=$2 WHERE id=$1")
        .bind(d.1)
        .bind(other_group)
        .execute(&state.pg)
        .await?;
    let mut td = target(d);
    td.provider_revision =
        sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1")
            .bind(td.provider_id)
            .fetch_one(&state.pg)
            .await?;
    health::register(state, &td, CallPurpose::Primary).await?;
    assert!(
        health::blocked(state, &td.reference(), CallPurpose::Primary)
            .await?
            .is_none(),
        "a declared project quota cannot poison another group sharing its key"
    );
    sqlx::query("DELETE FROM routing_health_scopes")
        .execute(&state.pg)
        .await?;
    sqlx::query("DELETE FROM routing_provider_health")
        .execute(&state.pg)
        .await?;
    sqlx::query("DELETE FROM routing_credential_health")
        .execute(&state.pg)
        .await?;
    let old = ta.clone();
    sqlx::query("UPDATE provider_configs SET label=label||'-renamed' WHERE id=$1")
        .bind(a.1)
        .execute(&state.pg)
        .await?;
    health::register(state, &ta, CallPurpose::Primary).await?; // Label is not auth rotation.
    use fosnie_backend::model_routing::vnext::connections;
    let connection = connections::list(state, admin)
        .await?
        .into_iter()
        .find(|r| r.id == a.1)
        .unwrap();
    // Use the real authorised rotation endpoint and its normal crypto store.
    let body = serde_json::json!({"schema_version":2,"expected_revision":connection.revision,"scope":"deployment","label":connection.label.unwrap(),"adapter":connection.adapter.unwrap(),"base_url":connection.base_url.unwrap(),"enabled":true,"quota_group_id":group,"metadata":connection.metadata,"api_key":"newer-synthetic-key"});
    connections::save(state, admin, a.1, serde_json::from_value(body)?).await?;
    ta.provider_revision =
        sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1")
            .bind(ta.provider_id)
            .fetch_one(&state.pg)
            .await?;
    ta.api_key = "newer-synthetic-key".into();
    health::register(state, &ta, CallPurpose::Primary).await?;
    health::note(
        state,
        &old,
        CallPurpose::Primary,
        &f(FailureKind::Unauthorized),
    )
    .await?;
    assert!(
        health::blocked(state, &ta.reference(), CallPurpose::Primary)
            .await?
            .is_none(),
        "late auth failure stays on the old connection/key"
    );
    health::note(
        state,
        &old,
        CallPurpose::Primary,
        &f(FailureKind::QuotaExhausted),
    )
    .await?;
    assert!(
        health::blocked(state, &ta.reference(), CallPurpose::Primary)
            .await?
            .is_some(),
        "key rotation cannot erase the captured shared account outage"
    );
    let status: serde_json::Value = serde_json::to_value(f(FailureKind::QuotaExhausted).public())?;
    assert!(!status.to_string().contains("synthetic-key") && status.get("binding_hash").is_none());
    Ok(())
}
