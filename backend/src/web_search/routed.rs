// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit inline web execution. General routed agents and durable deep jobs
//! remain gated until their entire orchestration adopts the same runtime.
use std::{collections::BTreeMap,time::Duration};
use serde_json::{Value,json};
use crate::{AppState,Result,ml,tools,ws::protocol::ServerFrame,
    model_routing::{run::RoutedRun,CallPurpose,RoutingError,WorkloadRequirements,
        dispatch::StageRequest,fixed::FixedRole,fixed_lease::FixedAllowance,transport::StreamOperation}};

/// Explicit administrator opt-in. Ordinary quick/standard retains its full pipeline.
pub(crate) async fn snippet_only(state:&AppState,depth:&str)->Result<bool> {
    if depth!="quick" {return Ok(false);}
    match crate::config::runtime::get(&state.pg,"web_search.quick_snippets_only").await? {
        None=>Ok(false),
        Some(v) if matches!(v.value_type,crate::config::runtime::ConfigValueType::Bool) && v.value=="true"=>Ok(true),
        Some(v) if matches!(v.value_type,crate::config::runtime::ConfigValueType::Bool) && v.value=="false"=>Ok(false),
        _=>Err(RoutingError::Invalid("invalid quick snippet mode").into()),
    }
}

/// The caller passes the original runtime and the turn's advertised tool set.
/// No user-ID reconstruction, implicit provider defaults or fresh budget.
pub async fn search(state:&AppState,run:&RoutedRun,authorised:&tools::AuthorisedTools,
    budget:Option<&tools::WebBudget>,args:&Value,tx:&tokio::sync::mpsc::Sender<ServerFrame>) -> Result<ml::WebSearchResult> {
    search_with_question(state,run,authorised,budget,args,"",tx).await
}

/// Routed chat supplies the original question. Existing callers keep their API
/// and rank quick excerpts against the engine query when no hint is available.
pub async fn search_with_question(state:&AppState,run:&RoutedRun,authorised:&tools::AuthorisedTools,
    budget:Option<&tools::WebBudget>,args:&Value,question:&str,tx:&tokio::sync::mpsc::Sender<ServerFrame>) -> Result<ml::WebSearchResult> {
    run.guard(Box::pin(async {
        let (chat,turn)=crate::chat::routing::authorize(state,run).await?;
        let context=run.context(CallPurpose::WebPlan);
        let overrides=tools::load_overrides(&state.pg).await?;
        match tools::authorize_native_call(state,&context.actor,chat,authorised,&overrides,"web_search",context.project_id).await {
            tools::NativeDecision::Allowed(_)=>{},
            _=>return Err(RoutingError::Forbidden.into()),
        }
        crate::integrations::guard_egress(state,&context.actor,crate::integrations::ConnectorKind::WebSearch).await?;
        let query=args.get("query").and_then(Value::as_str).map(str::trim).filter(|s|!s.is_empty())
            .ok_or(RoutingError::Invalid("web search needs a query"))?;
        let depth=tools::clamp_depth(args.get("depth").and_then(Value::as_str),budget.and_then(|b|b.depth_max.as_deref()));
        if depth=="deep" { return Err(RoutingError::Incompatible("deep web jobs require durable routed accounting").into()); }
        let snippets=snippet_only(state,&depth).await?;
        let recency=args.get("recency").and_then(Value::as_str).unwrap_or("any");
        if !matches!(recency,"any"|"day"|"week"|"month"|"year") { return Err(RoutingError::Invalid("web recency").into()); }
        let fetches=budget.and_then(|b|b.max_fetches).unwrap_or(8);
        if fetches<=0 { return Err(RoutingError::Invalid("web fetch cap").into()); }
        // Unlike legacy web_overrides, a DB error cannot erase domain restrictions.
        let mut body=json!({"query":query,"depth":depth,"recency":recency,"max_fetches":fetches.min(8)});
        if depth=="quick" { add_ranking_question(&mut body,question); }
        body["quick_snippets_only"]=json!(snippets);
        for (key,field) in [("web_search.allowlist","domain_allowlist"),("web_search.blocklist","domain_blocklist"),
            ("web_search.robots_policy","robots_policy"),("web_search.allowlist_only","allowlist_only")] {
            if let Some(entry)=crate::config::runtime::get(&state.pg,key).await? {
                if field=="robots_policy" && !matches!(entry.value.as_str(),"respect"|"user_triggered") {
                    return Err(RoutingError::Invalid("web robots policy").into());
                }
                body[field]=if field=="allowlist_only" { match entry.value.as_str() {
                    "true"=>json!(true),"false"=>json!(false),_=>return Err(RoutingError::Invalid("web allowlist mode").into()),
                }} else { json!(entry.value) };
            }
        }
        let mut stages=BTreeMap::new();
        let lanes=if snippets {vec![(CallPurpose::WebPlan,1,1)]} else {vec![(CallPurpose::WebPlan,8,512),(CallPurpose::WebGrade,16,80)]};
        for (purpose,calls,ceiling) in lanes {
            let (capacity,output)=run.capacity(state,purpose).await?;
            let output=output.min(ceiling);
            let input=capacity.checked_sub(output).filter(|n|*n>0).ok_or(RoutingError::Incompatible("web context capacity"))?;
            stages.insert(purpose,StageRequest { max_calls:calls,requirements:WorkloadRequirements {
                input_tokens:if snippets {input.min(512)}else{input},output_tokens:output,structured_output:!snippets,
                effort:run.plan().assignments[&purpose].effort.clone(),..Default::default()
            }});
        }
        let fixed=if snippets {BTreeMap::new()} else {BTreeMap::from([
            (FixedRole::Rerank,FixedAllowance { max_calls:32,input_bytes:1024*1024,output_bytes:8*1024*1024 }),
            (FixedRole::Ocr,FixedAllowance { max_calls:8,input_bytes:8*1024*1024,output_bytes:8*1024*1024 }),
        ])};
        // The snippet operation retains a bounded, accounted LLM lease but calls
        // no inference. A complete empty report releases unused reservations.
        let mut stream=run.stream(state,CallPurpose::WebPlan,StreamOperation::WebSearch,body,&stages,fixed,&Default::default(),Duration::from_secs(120)).await?;
        loop {
            let value=stream.recv().await?.ok_or(RoutingError::Invalid("web stream ended without terminal accounting"))?;
            let event:ml::WebEvent=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("web event"))?;
            match event {
                ml::WebEvent::Progress {stage,detail,..}=>{
                    let _=tx.send(ServerFrame::ChatTool {turn_id:turn,name:"web_search".into(),phase:"progress".into(),detail:Some(detail.unwrap_or(stage))}).await;
                }
                ml::WebEvent::Done {digest,citations}=>return Ok(ml::WebSearchResult {digest,citations}),
                _=>return Err(RoutingError::Invalid("unexpected inline web event").into()),
            }
        }
    })).await
}

// Only the original current-turn content supplied by chat execution is used.
// Tool arguments, history and fetched sources cannot replace this lexical hint.
fn add_ranking_question(body:&mut Value,question:&str) {
    let bounded:String=question.chars().take(2048).collect();
    if !bounded.trim().is_empty() { body["ranking_question"]=json!(bounded); }
}

#[cfg(test)]
mod quick_tests {
    use super::*;
    #[test]
    fn original_question_is_unicode_bounded_without_changing_search_scope() {
        let mut body=json!({"query":"public documentation","domain_allowlist":"trusted.example","max_fetches":2,
            "ranking_question":"model invented focus"});
        add_ranking_question(&mut body,&"Я🌦".repeat(2048));
        let value=body["ranking_question"].as_str().unwrap();
        assert_eq!(value.chars().count(),2048);
        assert!(value.starts_with("Я🌦"));
        assert_eq!(body["query"],"public documentation");
        assert_eq!(body["domain_allowlist"],"trusted.example");
        assert_eq!(body["max_fetches"],2);
        let mut blank=json!({"query":"public documentation"});
        add_ranking_question(&mut blank,"  ");
        assert!(blank.get("ranking_question").is_none());
    }
}
