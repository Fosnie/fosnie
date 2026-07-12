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

//! Runtime-mutable config.
//!
//! Typed, validated key/value records in `config_settings` — never one blob,
//! and validated on write. Every change writes a `config.changed` event into
//! the audit hash-chain, **atomically** with the config write (same
//! transaction), realising "every config change → audit".

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{AppError, Result};

/// Declared type of a config value. Maps to the `config_value_type` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "config_value_type", rename_all = "lowercase")]
pub enum ConfigValueType {
    String,
    Int,
    Float,
    Bool,
    Json,
}

impl ConfigValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigValueType::String => "string",
            ConfigValueType::Int => "int",
            ConfigValueType::Float => "float",
            ConfigValueType::Bool => "bool",
            ConfigValueType::Json => "json",
        }
    }
}

/// A stored config entry.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub value: String,
    pub value_type: ConfigValueType,
}

/// Validate that `value` parses as `value_type`. (Validation on write.)
/// Key-specific validators can be layered on later.
fn validate_value(value: &str, value_type: ConfigValueType) -> Result<()> {
    let ok = match value_type {
        ConfigValueType::String => true,
        ConfigValueType::Int => value.parse::<i64>().is_ok(),
        ConfigValueType::Float => value.parse::<f64>().is_ok(),
        ConfigValueType::Bool => matches!(value, "true" | "false"),
        ConfigValueType::Json => serde_json::from_str::<serde_json::Value>(value).is_ok(),
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "value {value:?} is not a valid {}",
            value_type.as_str()
        )))
    }
}

/// Read a config entry by key.
pub async fn get(pool: &PgPool, key: &str) -> Result<Option<ConfigEntry>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT value, value_type AS "value_type: ConfigValueType"
           FROM config_settings WHERE key = $1"#,
        key
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| ConfigEntry {
        value: r.value,
        value_type: r.value_type,
    }))
}

/// Upsert a config setting (validate → write → audit), atomically.
pub async fn set(
    pool: &PgPool,
    key: &str,
    value: &str,
    value_type: ConfigValueType,
    scope: &str,
    actor_user_id: Option<Uuid>,
    actor_role: &str,
) -> Result<()> {
    validate_value(value, value_type)?;

    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"
        INSERT INTO config_settings (key, value, value_type, scope, updated_by, updated_at)
        VALUES ($1, $2, $3, $4, $5, now())
        ON CONFLICT (key) DO UPDATE
            SET value = EXCLUDED.value,
                value_type = EXCLUDED.value_type,
                scope = EXCLUDED.scope,
                updated_by = EXCLUDED.updated_by,
                updated_at = now()
        "#,
        key,
        value,
        value_type as ConfigValueType,
        scope,
        actor_user_id,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("config.changed", actor_role);
    event.actor_user_id = actor_user_id;
    event.resource_type = Some("config_setting".into());
    event.payload = Some(json!({
        "key": key,
        "value": value,
        "value_type": value_type.as_str(),
        "scope": scope,
    }));
    audit::append_with(&mut tx, &event).await?;

    tx.commit().await?;
    Ok(())
}

/// Remove a knob's override row so it reverts to the ML/boot default. No-op when
/// the key was never set. Audited as `config.changed` in the
/// same transaction, mirroring [`set`].
pub async fn unset(pool: &PgPool, key: &str, actor_role: &str) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query!("DELETE FROM config_settings WHERE key = $1", key)
        .execute(&mut *tx)
        .await?;

    let mut event = AuditEvent::action("config.changed", actor_role);
    event.resource_type = Some("config_setting".into());
    event.payload = Some(json!({ "key": key, "reset": true }));
    audit::append_with(&mut tx, &event).await?;

    tx.commit().await?;
    Ok(())
}
