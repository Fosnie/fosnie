// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use serde_json::{json, Value};
use sqlx::PgConnection;
use uuid::Uuid;

pub async fn exercise(root: &mut PgConnection, owner: Uuid, provider: Uuid) -> anyhow::Result<(Uuid, Value)> {
    let old = Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,llm_provider_id) VALUES($1,$2,$3)")
        .bind(old).bind(owner).bind(provider).execute(&mut *root).await?;
    let legacy: Value = sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1")
        .bind(old).fetch_one(&mut *root).await?;
    assert_eq!(legacy, json!({"mode":"manual","provider_id":provider}));
    let id = Uuid::now_v7();
    let auto = json!({"schema_version":2,"required_profile_version":2,
        "selection":{"mode":"auto","objective":"economy"}});
    sqlx::query("INSERT INTO chats(id,owner_user_id,model_selection,llm_provider_id) VALUES($1,$2,$3,$4)")
        .bind(id).bind(owner).bind(&auto).bind(provider).execute(&mut *root).await?;
    let pointer: Option<Uuid> = sqlx::query_scalar("SELECT llm_provider_id FROM chats WHERE id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(pointer, None);
    let temporary = Uuid::now_v7();
    sqlx::query("INSERT INTO provider_configs(id,role,scope,label,model,base_url) VALUES($1,'llm','deployment','Binding fixture','binding-fixture','http://127.0.0.1:1/v1')")
        .bind(temporary).execute(&mut *root).await?;
    let manual = json!({"schema_version":2,"required_profile_version":1,
        "selection":{"mode":"manual","provider_id":temporary}});
    sqlx::query("UPDATE chats SET model_selection=$2 WHERE id=$1")
        .bind(id).bind(&manual).execute(&mut *root).await?;
    let pointer: Option<Uuid> = sqlx::query_scalar("SELECT llm_provider_id FROM chats WHERE id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(pointer, Some(temporary));
    sqlx::query("DELETE FROM provider_configs WHERE id=$1").bind(temporary).execute(&mut *root).await?;
    let stored: (Value, Option<Uuid>) = sqlx::query_as("SELECT model_selection,llm_provider_id FROM chats WHERE id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(stored, (manual.clone(), None), "retirement must preserve unavailable versioned intent");
    // Compatibility pointer writes cannot silently replace a v2 binding.
    sqlx::query("UPDATE chats SET llm_provider_id=$2 WHERE id=$1")
        .bind(id).bind(provider).execute(&mut *root).await?;
    let stored: Value = sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(stored, manual);
    for invalid in [json!({"schema_version":2,"selection":{"mode":"auto"}}),
        json!({"schema_version":2,"required_profile_version":3,"selection":{"mode":"auto"}}),
        json!({"schema_version":2,"required_profile_version":2,"selection":{"mode":"unknown"}})] {
        assert!(sqlx::query("UPDATE chats SET model_selection=$2 WHERE id=$1")
            .bind(id).bind(invalid).execute(&mut *root).await.is_err());
    }
    Ok((id, manual))
}

pub async fn rollback(root: &mut PgConnection, id: Uuid, intent: &Value) -> anyhow::Result<()> {
    let stored: Value = sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(&stored, intent);
    assert!(serde_json::from_value::<fosnie_backend::model_routing::ModelSelection>(stored).is_err(),
        "old clients must fail closed rather than infer a default");
    let archived: Value = sqlx::query_scalar("SELECT payload->'routing_intent' FROM routing_vnext_metadata_archive WHERE kind='chat_binding' AND entity_id=$1")
        .bind(id).fetch_one(&mut *root).await?;
    assert_eq!(&archived, intent);
    // The retained shape/compatibility functions also permit forward re-upgrade.
    sqlx::raw_sql(include_str!("../../migrations/0147_chat_routing_intent.sql")).execute(&mut *root).await?;
    Ok(())
}
