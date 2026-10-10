// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Bounded document executor for routed chat and explicit callers. All inference
//! uses the caller's original run and ledger.
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{AppState,Result,tools,model_routing::{run::RoutedRun,CallPurpose,RoutingError,
    WorkloadRequirements,dispatch::StageRequest,fixed::FixedRole,fixed_lease::FixedAllowance,
    transport::JsonOperation}};

#[derive(Debug,Deserialize,Serialize)]
pub struct DocumentRead {
    pub mode:String,
    pub text:String,
    pub truncated:bool,
}

/// Project link AND personal KB access AND the extension's document ACL are
/// required, even if the chat itself is readable. Re-run after ML before release.
async fn source(state:&AppState,run:&RoutedRun,authorised:&tools::AuthorisedTools,id:Uuid) -> Result<(String,Option<String>)> {
    let (chat,_)=crate::chat::routing::authorize(state,run).await?;
    let context=run.context(CallPurpose::Document);
    let overrides=tools::load_overrides(&state.pg).await?;
    if !matches!(tools::authorize_native_call(state,&context.actor,chat,authorised,&overrides,"read_document",context.project_id).await,
        tools::NativeDecision::Allowed(_)) {return Err(RoutingError::Forbidden.into());}
    let project=context.project_id.ok_or(RoutingError::Forbidden)?;
    let agent:Option<Uuid>=sqlx::query_scalar("SELECT agent_id FROM chats WHERE id=$1").bind(chat).fetch_one(&state.pg).await?;
    let allowed=crate::kb::retrieval_allowlist(&state.pg,&context.actor,chat,Some(project),agent).await?;
    let doc:Option<(Uuid,String,Option<String>)>=sqlx::query_as("SELECT d.kb_id,d.bytes_path,d.mime FROM kb_documents d JOIN project_kb_links p ON p.kb_id=d.kb_id WHERE d.id=$1 AND p.project_id=$2")
        .bind(id).bind(project).fetch_optional(&state.pg).await?;
    let Some((kb,path,mime))=doc else {return Err(RoutingError::Forbidden.into());};
    if !allowed.contains(&kb) || state.rbac.denied_kb_doc_ids(&state.pg,&context.actor,&[kb]).await?.contains(&id) {
        return Err(RoutingError::Forbidden.into());
    }
    Ok((path,mime))
}

pub async fn read(state:&AppState,run:&RoutedRun,authorised:&tools::AuthorisedTools,args:&Value) -> Result<DocumentRead> {
    let timeout=tools::timeout_for("read_document",&state.boot.tool_timeout_secs);
    tokio::time::timeout(timeout,run.guard(Box::pin(async {
        let id=args.get("doc_id").and_then(Value::as_str).and_then(|v|Uuid::parse_str(v).ok())
            .ok_or(RoutingError::Invalid("read_document needs a valid doc_id"))?;
        let query=match args.get("query") {
            None|Some(Value::Null)=>None,
            Some(Value::String(s)) if s.len()<=8192=>Some(s.trim()),
            _=>return Err(RoutingError::Invalid("invalid document query").into()),
        };
        let doc=source(state,run,authorised,id).await?;
        let path=crate::storage::resolve_file(&state.boot.storage.documents_dir,&doc.0);
        let (capacity,output)=run.capacity(state,CallPurpose::Document).await?;
        let output=output.min(512);
        let input=capacity.checked_sub(output).filter(|n|*n>2048).ok_or(RoutingError::Incompatible("document context capacity"))?;
        // Reserve headroom for the caller's other messages. This is a byte cap,
        // not a chars/4 token estimate; the eventual model call checks all input.
        let (final_capacity,_)=run.capacity(state,CallPurpose::ToolLoop).await?;
        let output_bytes=(final_capacity/4).min(16000).max(1);
        let stages=[(CallPurpose::Document,StageRequest {max_calls:32,requirements:WorkloadRequirements {
            input_tokens:input,output_tokens:output,effort:run.plan().assignments[&CallPurpose::Document].effort.clone(),..Default::default()
        }})].into();
        let fixed=[(FixedRole::Ocr,FixedAllowance {max_calls:1,input_bytes:8*1024*1024,output_bytes:8*1024*1024})].into();
        let body=json!({"path":path.to_string_lossy(),"mime":doc.1,"prompt":query,
            "document_limits":{"max_file_bytes":4*1024*1024,"output_bytes":output_bytes,"max_sections":32}});
        let value=run.json(state,CallPurpose::Document,JsonOperation::ReadDocument,body,&stages,fixed,&Default::default(),timeout).await?;
        let result:DocumentRead=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("invalid document result"))?;
        if !matches!(result.mode.as_str(),"stuff"|"map_reduce") || result.text.len()>output_bytes as usize {
            return Err(RoutingError::Invalid("document result exceeds contract").into());
        }
        if source(state,run,authorised,id).await?!=doc {return Err(RoutingError::NeedsReplan.into());}
        Ok(result)
    }))).await.map_err(|_|crate::AppError::Unavailable("routed document timeout".into()))?
}
