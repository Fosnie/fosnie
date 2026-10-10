// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Queue-time agent configuration, revalidated before routed work.
use serde_json::Value;
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::RoutingError};

pub(crate) async fn capture(conn:&mut sqlx::PgConnection,id:Uuid)->Result<Value> {
    Ok(sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('system_prompt',a.system_prompt,'params',a.params,'created_by',a.created_by,'modes',a.modes,'tools',COALESCE((SELECT jsonb_agg(tool_name ORDER BY tool_name) FROM agent_tools WHERE agent_id=a.id),'[]'::jsonb),'kb_ids',COALESCE((SELECT jsonb_agg(project_knowledge_id ORDER BY project_knowledge_id) FROM agent_project_knowledge WHERE agent_id=a.id),'[]'::jsonb),'skills',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',s.id,'name',s.name,'description',s.description) ORDER BY s.id) FROM skills s WHERE s.enabled AND (s.is_default OR EXISTS(SELECT 1 FROM agent_skills b WHERE b.agent_id=a.id AND b.skill_id=s.id))),'[]'::jsonb)) FROM agents a WHERE a.id=$1 AND a.archived_at IS NULL")
        .bind(id).fetch_optional(conn).await?.unwrap_or(Value::Null))
}
pub(super) async fn validate(state:&AppState,actor:&AuthContext,input:&Value)->Result<(Option<Uuid>,Vec<Uuid>)> {
    let id:Option<Uuid>=serde_json::from_value(input["agent_id"].clone()).map_err(|_|RoutingError::Invalid("workflow agent"))?;
    let Some(id)=id else {return Ok((None,vec![]));};
    let current=capture(&mut *state.pg.acquire().await?,id).await?;
    if current.is_null() || current!=input["agent_snapshot"] {return Err(RoutingError::NeedsReplan.into());}
    let owner:Option<Uuid>=serde_json::from_value(current["created_by"].clone()).map_err(|_|RoutingError::Forbidden)?;
    if !actor.is_admin() && owner.is_some() && owner!=actor.user_id {return Err(RoutingError::Forbidden.into());}
    if !current["modes"].as_array().is_some_and(|v|v.iter().any(|m|m=="general")) {return Err(RoutingError::Incompatible("workflow agent must support general mode").into());}
    let ids=serde_json::from_value(current["kb_ids"].clone()).map_err(|_|RoutingError::Invalid("agent libraries"))?;
    Ok((Some(id),ids))
}

/// Outer workflow cancellation may drop chat::run_turn before its cleanup runs.
/// Include pre-checkpoint agent rows: start_run can fail after inserting a row.
pub(super) async fn cleanup(state:&AppState,run:Option<Uuid>)->Result<()> {
    let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE agent_runs a SET status='failed',finished_at=now(),updated_at=now() FROM workflow_runs w JOIN workflow_routing_inputs i ON i.run_id=w.id WHERE a.turn_id=w.id AND a.acting_user_id=w.run_as_user_id AND a.status='running' AND w.status IN ('failed','skipped') AND ($1::uuid IS NULL OR w.id=$1) RETURNING a.id")
        .bind(run).fetch_all(&state.pg).await?;
    for id in ids {crate::agent::release_token(state,id).await;}
    Ok(())
}
