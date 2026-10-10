// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One bounded clarification call under the existing ResearchPlan assignment.
use serde::{Deserialize,Serialize};
use serde_json::json;
use crate::{AppState,Result,model_routing::{*,run::RoutedRun,dispatch::StageRequest,transport::JsonOperation}};
use super::routed::Request;

#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {pub prompt:String,pub options:Vec<String>}
#[derive(Clone,Serialize,Deserialize)]
pub(super) struct Checkpoint {
    pub id:uuid::Uuid,pub questions:Vec<Question>,pub answers:Option<Vec<String>>,
}
pub(super) fn text_valid(s:&str,max:usize)->bool {
    !s.trim().is_empty() && s.len()<=max && !s.chars().any(char::is_control)
}
pub(super) async fn ask(state:&AppState,run:&RoutedRun,req:&Request)->Result<Vec<Question>> {
    let (capacity,output)=run.capacity(state,CallPurpose::ResearchPlan).await?;
    let output=output.min(1024);
    let input=capacity.checked_sub(output).filter(|n|*n>=8192)
        .ok_or(RoutingError::Incompatible("research triage model context capacity"))?.min(16384);
    let requirements=WorkloadRequirements {input_tokens:input,output_tokens:output,
        effort:run.plan().assignments[&CallPurpose::ResearchPlan].effort.clone(),..Default::default()};
    let body=json!({"question":req.question,"source":req.source,"library_count":req.kb_ids.len(),"refinements":req.refinements});
    let value=run.json(state,CallPurpose::ResearchPlan,JsonOperation::ResearchTriage,body,
        &[(CallPurpose::ResearchPlan,StageRequest {requirements,max_calls:1})].into(),
        Default::default(),&Default::default(),std::time::Duration::from_secs(120)).await?;
    let questions:Vec<Question>=serde_json::from_value(value["questions"].clone())
        .map_err(|_|RoutingError::Invalid("invalid research clarification"))?;
    if questions.len()>3 || questions.iter().any(|q|!text_valid(&q.prompt,240) || q.options.len()>4
        || q.options.iter().any(|s|!text_valid(s,256))
        || q.options.iter().collect::<std::collections::BTreeSet<_>>().len()!=q.options.len()) {
        return Err(RoutingError::Invalid("invalid research clarification").into());
    }
    Ok(questions)
}
