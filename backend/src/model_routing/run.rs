// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit runtime ownership for a planned turn. Clones share actor, scope,
//! pinned plan, ledger and sticky cancellation; no task-local inheritance.
use std::{collections::{BTreeMap,BTreeSet},future::Future,sync::Arc,time::Duration};
use serde_json::Value;
use tokio::sync::{Notify,watch};
use super::{*,budget::{RunBudget,Usage},dispatch::StageRequest,fixed::FixedRole,
    fixed_lease::FixedAllowance,transport::{PipelineClient,PipelineStream,JsonOperation,StreamOperation}};
#[path="run_audio.rs"]
mod audio;
#[path="run_fallback.rs"]
mod fallback_run;
#[path="run_quality.rs"]
mod quality_run;
pub use audio::{MAX_AUDIO_BYTES,MAX_SPEECH_TEXT_BYTES};

#[derive(Clone)]
pub struct RoutedRun(Arc<Inner>);
/// In-process caller witness. Never serialized or reconstructed from a reason
/// code supplied by a client. Durable callers retain their own persisted fence.
#[async_trait::async_trait]
pub(crate) trait AccessGuard: Send + Sync {
    async fn check(&self) -> crate::Result<()>;
    fn unattended(&self) -> bool { false }
    fn plain_text_retry(&self)->bool {false}
    fn phone_call(&self) -> Option<uuid::Uuid> {None}
    fn regeneration_chat(&self) -> Option<uuid::Uuid> {None}
    fn browser_chat(&self) -> Option<uuid::Uuid> {None}
    fn desktop_device(&self)->Option<uuid::Uuid> {None}
    fn selection_intent(&self) -> Option<ModelSelection> {None}
    fn routing_options(&self) -> Option<fosnie_protocol::RoutingOptions> {None}
}
struct Inner {
    context:RoutingContext,plan:ExecutionPlan,budget:RunBudget,client:PipelineClient,
    cancelled:watch::Sender<bool>,
    durable:Option<super::durable::Lease>,
    serial:Arc<tokio::sync::Mutex<()>>,
    access:Option<Arc<dyn AccessGuard>>,
    artefact:Option<crate::chat::routed_artefact::Intent>,
    children:std::sync::atomic::AtomicUsize,
    children_done:Notify,
    retry:Option<super::fallback::RetryRequirements>,
    browser_retry_claimed:Arc<std::sync::atomic::AtomicBool>,
    fallback:Option<super::fallback::Attempt>,
    quality:Option<super::quality::QualityAttempt>,
}
impl std::fmt::Debug for RoutedRun {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutedRun").field("run_id",&self.0.context.run_id)
            .field("decision_id",&self.0.plan.decision.id).finish_non_exhaustive()
    }
}
impl RoutedRun {
    pub(crate) fn plain_text_retry_allowed(&self)->bool {self.0.access.as_ref().is_some_and(|a|a.plain_text_retry())}
    pub(crate) fn browser_chat(&self)->Option<uuid::Uuid> {self.0.access.as_ref().and_then(|a|a.browser_chat())}
    pub(crate) fn desktop_device(&self)->Option<uuid::Uuid> {self.0.access.as_ref().and_then(|a|a.desktop_device())}
    pub(crate) fn selection_intent(&self)->ModelSelection {self.0.access.as_ref().and_then(|a|a.selection_intent()).unwrap_or_else(||self.0.plan.selection.clone())}
    pub(crate) fn routing_options(&self)->Option<fosnie_protocol::RoutingOptions> {self.0.access.as_ref().and_then(|a|a.routing_options())}
    pub(crate) fn regeneration_chat(&self)->Option<uuid::Uuid> {self.0.access.as_ref().and_then(|a|a.regeneration_chat())}
    pub(crate) fn phone_call(&self)->Option<uuid::Uuid> {self.0.access.as_ref().and_then(|a|a.phone_call())}
    /// Call after authorizing the conversation and filtering source/history ACLs.
    /// Classifier spend stays in the SAME ledger used by every subsequent helper.
    /// This is a new run, not a job-resume API: restoring a plan alone must never
    /// reset a previous run's consumed budget or cancellation state.
    pub async fn auto(state:&crate::AppState,context:RoutingContext,policy_id:Option<uuid::Uuid>,
        objective:RoutingObjective,requirements:WorkloadRequirements,conversation:&str,
        fixed_roles:&[FixedRole],cancel:&Notify) -> crate::Result<Self> {
        Self::selected(state,context,ModelSelection::Auto {objective,policy_id},requirements,conversation,fixed_roles,cancel).await
    }
    pub async fn selected(state:&crate::AppState,context:RoutingContext,selection:ModelSelection,
        requirements:WorkloadRequirements,conversation:&str,fixed_roles:&[FixedRole],cancel:&Notify)->crate::Result<Self> {
        if context.actor.mfa_enroll_only { return Err(RoutingError::Forbidden.into()); }
        tokio::select! { biased;
            _=cancel.notified()=>return Err(RoutingError::Cancelled.into()),
            result=async {
                let never_cancel=Notify::new();
                let (mut plan,budget,retry)=service::selected_plan_with_retry(state,&context,selection,requirements,conversation,&never_cancel,None).await?;
                fixed_lease::pin(state,&context,&mut plan,fixed_roles).await?;
                let mut run=Self::from_planned(context,plan,budget,PipelineClient::from_state(state)?)?;
                Arc::get_mut(&mut run.0).unwrap().retry=retry;
                Ok(run)
            }=>result,
        }
    }
    /// New, request-owned evaluation only. The caller supplies narrower limits,
    /// never a restored ledger. Apply them before the first classifier call.
    pub(crate) async fn selected_capped(state:&crate::AppState,context:RoutingContext,selection:ModelSelection,
        requirements:WorkloadRequirements,conversation:&str,expected_policy:(uuid::Uuid,i64),limits:RoutingConstraints)->crate::Result<Self> {
        let id=match &selection {ModelSelection::Auto{policy_id,..}=>*policy_id,_=>None};
        let policy=store::load_policy(state,&context.actor,id).await?;
        if (policy.id,policy.revision)!=expected_policy {return Err(RoutingError::NeedsReplan.into());}
        let ledger=service::PlanningLedger{policy_id:policy.id,policy_revision:policy.revision,budget:RunBudget::new(limits)};
        let (plan,budget,_)=service::selected_plan_with_retry(state,&context,selection,requirements,conversation,&Notify::new(),Some(ledger)).await?;
        Self::from_planned(context,plan,budget,PipelineClient::from_state(state)?)
    }
    /// Fixed-only maintenance owns the same policy/ledger without ranking an
    /// LLM or paying for a classifier that cannot choose embedding spaces.
    pub(crate) async fn fixed_rebuild(state:&crate::AppState,context:RoutingContext)->crate::Result<Self> {
        let policy=store::load_policy(state,&context.actor,None).await?;
        engine::validate_policy(&policy)?;
        let mut constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints)
            .intersect(&RoutingConstraints {max_calls:Some(512),max_duration_ms:Some(1_800_000),..Default::default()});
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&context).await?);
        let mut plan=ExecutionPlan {id:context.run_id,schema_version:SCHEMA_VERSION,actor_user_id:context.actor.user_id,
            selection:ModelSelection::Default,decision:service::empty_decision(&context,&policy,"fixed_embedding_rebuild"),
            constraints,assignments:Default::default(),fixed_assignments:Default::default()};
        fixed_lease::pin(state,&context,&mut plan,&[FixedRole::Embed,FixedRole::EmbedDesired]).await?;
        let budget=RunBudget::new(plan.constraints.clone());
        Self::runtime(context,plan,budget,PipelineClient::from_state(state)?)
    }
    fn is_fixed_rebuild(context:&RoutingContext,plan:&ExecutionPlan)->bool {
        context.purpose==CallPurpose::Embed && plan.assignments.is_empty()
            && plan.decision.reason_codes==["fixed_embedding_rebuild"]
            && plan.fixed_assignments.keys().copied().collect::<BTreeSet<_>>()==[FixedRole::Embed,FixedRole::EmbedDesired].into()
    }
    // Only planning code may assemble a runtime. Product callers cannot supply a
    // fresh budget for an existing plan through a public deserialization API.
    pub(crate) fn from_planned(context:RoutingContext,plan:ExecutionPlan,budget:RunBudget,client:PipelineClient) -> crate::Result<Self> {
        if plan.schema_version!=SCHEMA_VERSION || plan.id!=context.run_id || plan.actor_user_id!=context.actor.user_id {
            return Err(RoutingError::Forbidden.into());
        }
        if context.actor.mfa_enroll_only { return Err(RoutingError::Forbidden.into()); }
        if Self::is_fixed_rebuild(&context,&plan) {return Self::runtime(context,plan,budget,client);}
        let primary=plan.assignments.get(&CallPurpose::Primary).ok_or(RoutingError::Invalid("missing primary assignment"))?;
        let same=|other:&TargetReference|other.provider_id==primary.provider_id && other.provider_revision==primary.provider_revision
            && other.profile_revision==primary.profile_revision && other.model==primary.model && other.effort==primary.effort;
        if [CallPurpose::ToolLoop,CallPurpose::Final].iter().any(|p|!plan.assignments.get(p).is_some_and(same)) {
            return Err(RoutingError::Invalid("primary/tool/final assignments must stay pinned").into());
        }
        Self::runtime(context,plan,budget,client)
    }
    fn runtime(context:RoutingContext,plan:ExecutionPlan,budget:RunBudget,client:PipelineClient)->crate::Result<Self> {
        budget.restrict(&plan.constraints);
        budget.check_deadline()?;
        let (cancelled,_)=watch::channel(false);
        Ok(Self(Arc::new(Inner { context,plan,budget,client,cancelled,durable:None,serial:Arc::new(tokio::sync::Mutex::new(())),
            access:None,artefact:None,children:std::sync::atomic::AtomicUsize::new(0),children_done:Notify::new(),retry:None,fallback:None,quality:None,browser_retry_claimed:Arc::new(std::sync::atomic::AtomicBool::new(false)) })))
    }
    /// Adopt fixed stages before a durable handoff; preserve the original ledger.
    pub(crate) async fn adopt_fixed_roles(mut self,state:&crate::AppState,roles:&[FixedRole])->crate::Result<Self> {
        let inner=Arc::get_mut(&mut self.0).ok_or(RoutingError::Incompatible("fixed adoption requires exclusive runtime ownership"))?;
        if inner.durable.is_some() || !inner.budget.exclusive() {return Err(RoutingError::Incompatible("fixed adoption requires an idle new run").into());}
        fixed_lease::pin(state,&inner.context,&mut inner.plan,roles).await?;
        inner.budget.restrict(&inner.plan.constraints);self.check()?;Ok(self)
    }
    pub(crate) fn freeze(self)->crate::Result<super::durable::Frozen> {
        self.check()?;
        if self.browser_chat().is_some() {return Err(RoutingError::Incompatible("browser chat cannot be resumed as a durable job").into());}
        if self.0.plan.assignments.is_empty() && !Self::is_fixed_rebuild(&self.0.context,&self.0.plan) {return Err(RoutingError::Incompatible("fixed voice runs are request-owned").into());}
        if self.0.durable.is_some() || self.0.access.is_some() || self.0.artefact.is_some() || !self.0.budget.exclusive() {return Err(RoutingError::Incompatible("run has outstanding inference, an in-process access witness or is already durable").into());}
        let inner=Arc::try_unwrap(self.0).map_err(|_|RoutingError::Incompatible("queue handoff requires exclusive runtime ownership"))?;
        Ok(super::durable::Frozen {context:super::durable::Context::capture(&inner.context)?,plan:inner.plan,
            limits:inner.budget.limits(),usage:inner.budget.snapshot(),deadline:time::OffsetDateTime::now_utc()+time::Duration::milliseconds(inner.budget.remaining(Duration::from_secs(86400)).as_millis() as i64)})
    }
    pub(crate) fn from_durable(context:RoutingContext,plan:ExecutionPlan,budget:RunBudget,client:PipelineClient,lease:super::durable::Lease)->crate::Result<Self> {
        let mut run=Self::from_planned(context,plan,budget,client)?;
        Arc::get_mut(&mut run.0).unwrap().durable=Some(lease);Ok(run)
    }
    pub(crate) fn durable_id(&self)->Option<uuid::Uuid> {self.0.durable.as_ref().map(|l|l.id)}
    pub(crate) async fn fence(&self,tx:&mut sqlx::Transaction<'_,sqlx::Postgres>)->crate::Result<()> {
        self.check()?;
        if let Some(lease)=&self.0.durable {lease.fence(tx).await?;} Ok(())
    }
    pub fn context(&self,purpose:CallPurpose) -> RoutingContext {
        let mut context=self.0.context.clone(); context.purpose=purpose; context
    }
    pub fn plan(&self) -> &ExecutionPlan { &self.0.plan }
    pub(crate) fn artefact_intent(&self)->Option<&crate::chat::routed_artefact::Intent> {self.0.artefact.as_ref()}
    pub(crate) fn with_artefact(mut self,intent:crate::chat::routed_artefact::Intent)->crate::Result<Self> {
        Arc::get_mut(&mut self.0).ok_or(RoutingError::Incompatible("runtime is already shared"))?.artefact=Some(intent);Ok(self)
    }
    pub(crate) fn with_access(mut self,guard:Arc<dyn AccessGuard>)->crate::Result<Self> {
        Arc::get_mut(&mut self.0).ok_or(RoutingError::Incompatible("runtime is already shared"))?.access=Some(guard);
        Ok(self)
    }
    pub(crate) fn allows_unattended(&self)->bool {self.0.access.as_ref().is_some_and(|g|g.unattended())}
    pub(crate) async fn validate_access(&self)->crate::Result<()> {
        self.check()?;
        if let Some(guard)=&self.0.access {guard.check().await?;}
        if let Some(intent)=&self.0.artefact {intent.check().await?;} Ok(())
    }
    pub(crate) async fn finish_children(&self)->crate::Result<()> {
        let result=self.guard(self.wait_children()).await;
        if result.is_err() {
            self.cancel();
            // Cancellation wakes every child guard. Settle their dropped leases
            // before recording the final conservative usage snapshot.
            let _=tokio::time::timeout(Duration::from_secs(5),self.wait_children()).await;
        }
        result
    }
    async fn wait_children(&self)->crate::Result<()> {
        loop {
            let notified=self.0.children_done.notified();tokio::pin!(notified);notified.as_mut().enable();
            if self.0.children.load(std::sync::atomic::Ordering::SeqCst)==0 {return Ok(());}
            notified.await;
        }
    }
    pub(crate) fn add_handoff_reason(&mut self,reason:&str)->crate::Result<()> {
        let inner=Arc::get_mut(&mut self.0).ok_or(RoutingError::Incompatible("runtime is already shared"))?;
        inner.plan.decision.reason_codes.push(reason.into());Ok(())
    }
    pub fn usage(&self) -> Usage { self.0.budget.snapshot() }
    pub fn validate_turn(&self,actor:&crate::auth::AuthContext,turn_id:uuid::Uuid,chat_id:Option<uuid::Uuid>) -> crate::Result<()> {
        let original=&self.0.context;
        if original.actor.user_id!=actor.user_id || original.actor.role!=actor.role || original.actor.break_glass!=actor.break_glass
            || original.actor.mfa_enroll_only!=actor.mfa_enroll_only || original.run_id!=turn_id || original.chat_id!=chat_id || chat_id.is_none() {
            return Err(RoutingError::Forbidden.into());
        }
        self.check()
    }
    // A browser response ceiling constrains generative stages, not unknown-token
    // fixed services. Explicit policy token/money caps remain in the shared ledger.
    fn surface_output(&self, output:u32)->u32 {
        if self.browser_chat().is_some() {output.min(crate::chat::browser::OUTPUT_TOKENS)} else {output}
    }
    fn validate_surface_stages(&self,stages:&BTreeMap<CallPurpose,StageRequest>)->crate::Result<()> {
        if stages.values().any(|s|s.requirements.output_tokens>self.surface_output(s.requirements.output_tokens)) {
            return Err(RoutingError::BudgetExhausted.into());
        }Ok(())
    }
    /// Read the pinned model's real capacity, never the ML process default.
    pub async fn capacity(&self,state:&crate::AppState,purpose:CallPurpose) -> crate::Result<(u32,u32)> {
        self.check()?;
        let reference=self.0.plan.assignments.get(&purpose).ok_or(RoutingError::Invalid("unplanned purpose"))?;
        let profiles=store::current_profiles(store::catalog(state,&self.0.context.actor).await?);
        let profile=profiles.iter().find(|p|p.provider_id==reference.provider_id && p.provider_revision==reference.provider_revision
            && p.revision==reference.profile_revision && p.model==reference.model).ok_or(RoutingError::NeedsReplan)?;
        Ok((profile.capabilities.context_tokens.ok_or(RoutingError::Incompatible("unknown context capacity"))?,
            self.surface_output(profile.capabilities.max_output_tokens.ok_or(RoutingError::Incompatible("unknown output capacity"))?)))
    }
    pub async fn generate(&self,state:&crate::AppState,mut request:crate::ml::GenerateRequest) -> crate::Result<RoutedStream> {
        let purpose=CallPurpose::Final;
        let reference=self.plan().assignments.get(&purpose).ok_or(RoutingError::Invalid("unplanned final purpose"))?;
        if request.model.as_ref().is_some_and(|model|model!=&reference.model) { return Err(RoutingError::Incompatible("model differs from plan").into()); }
        if request.sampling.reasoning_effort.is_none() { request.sampling.reasoning_effort=reference.effort.clone(); }
        let requirements=dispatch::call_requirements(reference,&request.messages,request.tools.as_deref(),&request.sampling)?;
        let body=serde_json::to_value(request).map_err(|_|RoutingError::Invalid("generation request"))?;
        self.stream(state,purpose,StreamOperation::Generate,body,&[(purpose,StageRequest { requirements,max_calls:1 })].into(),
            Default::default(),&Default::default(),Duration::from_secs(120)).await
    }
    pub fn cancel(&self) { self.0.cancelled.send_replace(true); }
    pub fn is_cancelled(&self) -> bool { *self.0.cancelled.borrow() }
    async fn cancelled(&self) {
        let mut receiver=self.0.cancelled.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() { break; }
        }
    }
    fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() { return Err(RoutingError::Cancelled.into()); }
        if self.usage().exhausted { return Err(RoutingError::BudgetExhausted.into()); }
        self.0.budget.check_deadline()?; Ok(())
    }
    /// Guard the entire helper, including metadata preparation and response read.
    pub async fn guard<T>(&self,future:impl Future<Output=crate::Result<T>>) -> crate::Result<T> {
        self.check()?;
        tokio::select! { biased;
            _=self.cancelled()=>Err(RoutingError::Cancelled.into()),
            result=tokio::time::timeout(self.0.budget.remaining(Duration::from_secs(86_400)),future)=>
                result.map_err(|_|crate::AppError::Unavailable("routed run deadline exceeded".into()))?,
        }
    }
    /// Explicit child handoff: captures the original actor, plan and ledger.
    /// No ambient task-local or reconstructed user ID is used in the child.
    pub fn spawn<T,F,Fut>(&self,work:F) -> tokio::task::JoinHandle<crate::Result<T>>
    where T:Send+'static,F:FnOnce(Self)->Fut+Send+'static,Fut:Future<Output=crate::Result<T>>+Send+'static {
        self.0.children.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let child=Child(self.clone());
        tokio::spawn(async move {let run=&child.0;run.guard(async {work(run.clone()).await}).await})
    }
    /// Bridge a turn's existing cancellation signal into sticky broadcast state.
    pub async fn drive<T>(&self,cancel:&Notify,future:impl Future<Output=crate::Result<T>>) -> crate::Result<T> {
        tokio::select! { biased;
            _=cancel.notified()=>{ self.cancel(); Err(RoutingError::Cancelled.into()) },
            result=self.guard(future)=>result,
        }
    }
    pub async fn json(&self,state:&crate::AppState,purpose:CallPurpose,operation:JsonOperation,body:Value,
        stages:&BTreeMap<CallPurpose,StageRequest>,fixed:BTreeMap<FixedRole,FixedAllowance>,
        references:&BTreeSet<FixedRole>,timeout:Duration) -> crate::Result<Value> {
        self.validate_surface_stages(stages)?;
        // Mixed tool loops can nest several large state machines. Heap ownership
        // bounds Windows stack use without spawning work or changing cancellation.
        self.guard(Box::pin(async {
            let _serial=if self.0.durable.is_some() {Some(self.0.serial.lock().await)} else {None};
            let prepared=Box::pin(mixed::prepare(state,&self.context(purpose),&self.0.plan,&self.0.budget,stages,fixed,references,timeout)).await?;
            self.validate_access().await?;
            Box::pin(crate::workflows::sources::validate_run(state,self)).await?;
            if let Some(lease)=&self.0.durable {lease.checkpoint(&self.0.budget).await?;}
            let result=self.0.client.json(operation,body,prepared,&Notify::new()).await;
            if let Some(lease)=&self.0.durable {lease.checkpoint(&self.0.budget).await?;}
            result
        })).await
    }
    pub async fn stream(&self,state:&crate::AppState,purpose:CallPurpose,operation:StreamOperation,body:Value,
        stages:&BTreeMap<CallPurpose,StageRequest>,fixed:BTreeMap<FixedRole,FixedAllowance>,
        references:&BTreeSet<FixedRole>,timeout:Duration) -> crate::Result<RoutedStream> {
        self.validate_surface_stages(stages)?;
        self.guard(Box::pin(async {
            // Keep the lease's serialization lock for the whole response, not
            // merely until headers arrive. No sibling can checkpoint a refund
            // while this request still has unreported work.
            let serial=if self.0.durable.is_some() {Some(self.0.serial.clone().lock_owned().await)} else {None};
            let prepared=Box::pin(mixed::prepare(state,&self.context(purpose),&self.0.plan,&self.0.budget,stages,fixed,references,timeout)).await?;
            self.validate_access().await?;
            Box::pin(crate::workflows::sources::validate_run(state,self)).await?;
            self.open_stream(operation,body,prepared,serial).await
        })).await
    }
    async fn open_stream(&self,operation:StreamOperation,body:Value,prepared:mixed::PreparedMixedRequest,
        serial:Option<tokio::sync::OwnedMutexGuard<()>>)->crate::Result<RoutedStream> {
        if let Some(lease)=&self.0.durable {lease.checkpoint(&self.0.budget).await?;}
        let stream=self.0.client.stream(operation,body,prepared,&Notify::new()).await?;
        Ok(RoutedStream {run:self.clone(),stream:Some(stream),serial})
    }
    pub async fn chat_step(&self,state:&crate::AppState,purpose:CallPurpose,messages:&[crate::ml::Message],
        tools:Option<&[Value]>,sampling:&crate::ml::Sampling) -> crate::Result<crate::ml::ChatStep> {
        let reference=self.0.plan.assignments.get(&purpose).ok_or(RoutingError::Invalid("unplanned LLM purpose"))?;
        let mut sampling=sampling.clone();
        if sampling.reasoning_effort.is_none() { sampling.reasoning_effort=reference.effort.clone(); }
        let requirements=dispatch::call_requirements(reference,messages,tools,&sampling)?;
        let body=serde_json::json!({"messages":messages,"tools":tools,"sampling":sampling});
        let value=self.json(state,purpose,JsonOperation::ChatStep,body,
            &[(purpose,StageRequest { requirements,max_calls:1 })].into(),Default::default(),&Default::default(),Duration::from_secs(120)).await?;
        let usage=reported_usage(&value,purpose)?;
        let mut step:crate::ml::ChatStep=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("invalid routed chat-step response"))?;
        step.usage=usage;Ok(step)
    }
}

struct Child(RoutedRun);
/// Completion-facing counters come from the validated lease report as well as
/// the run ledger, never a contradictory top-level provider usage object.
pub(crate) fn reported_usage(value:&Value,purpose:CallPurpose)->crate::Result<crate::ml::Usage> {
    let calls=value["routing_report"]["calls"].as_array().filter(|c|c.len()==1)
        .ok_or(RoutingError::Invalid("expected one reported LLM call"))?;
    let call=&calls[0];
    if call["purpose"]!=serde_json::json!(purpose) || call["status"]!="completed" {return Err(RoutingError::Invalid("LLM call did not complete").into());}
    let count=|key:&str|->crate::Result<Option<i32>> {
        match call["usage"][key].as_u64() {
            Some(n)=>Ok(Some(i32::try_from(n).map_err(|_|RoutingError::Invalid("token count exceeds completion range"))?)),
            None=>Ok(None),
        }
    };
    Ok(crate::ml::Usage {prompt_tokens:count("input_tokens")?,completion_tokens:count("output_tokens")?,reasoning_tokens:count("reasoning_tokens")?})
}
impl Drop for Child {
    fn drop(&mut self) {
        self.0.0.children.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);
        self.0.0.children_done.notify_waiters();
    }
}

pub struct RoutedStream {
    run:RoutedRun,stream:Option<PipelineStream>,serial:Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl RoutedStream {
    pub async fn recv(&mut self) -> crate::Result<Option<Value>> {
        let Some(stream)=self.stream.as_mut() else { return Ok(None); };
        let result=self.run.guard(async {
            let result=stream.recv(&Notify::new()).await;
            if stream.is_finished() || result.is_err() {
                // A terminal event is not visible to the worker until its
                // accounting is durable. Interruption keeps the pre-send charge.
                if let Some(lease)=&self.run.0.durable {lease.checkpoint(&self.run.0.budget).await?;}
            }
            result
        }).await;
        if stream.is_finished() || !matches!(&result,Ok(Some(_))) {
            self.stream.take(); self.serial.take();
        }
        result
    }
}

#[cfg(test)]
#[path="run_tests.rs"]
mod tests;
