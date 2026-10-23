// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Fixed audio calls share the run's ledger; they never invoke an LLM judge.
use super::*;
use super::super::transport::AudioResponse;

pub const MAX_AUDIO_BYTES:usize=16*1024*1024;
pub const MAX_SPEECH_TEXT_BYTES:usize=64*1024;
enum Request {Transcribe {audio:Vec<u8>,mime:String,language:Option<String>},Speech {text:String,voice:Option<String>,wav:bool}}
enum Response {Text(Value),Audio(AudioResponse)}

impl RoutedRun {
    /// Standalone dictation/read-aloud does not need a configured chat model.
    /// Combined conversational runs must use their original selected runtime.
    pub async fn fixed_voice(state:&crate::AppState,context:RoutingContext,roles:&[FixedRole],cancel:&Notify)->crate::Result<Self> {
        Self::pending_voice(state,context,ModelSelection::Default,roles,cancel).await
    }
    /// Pin the intended policy and audio engines BEFORE receiving a transcript.
    /// No classifier or chat-model inference runs at this phase.
    pub async fn pending_voice(state:&crate::AppState,context:RoutingContext,selection:ModelSelection,roles:&[FixedRole],cancel:&Notify)->crate::Result<Self> {
        if context.actor.user_id.is_none() || context.actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
        if roles.is_empty() || roles.iter().any(|r|!matches!(r,FixedRole::Stt|FixedRole::Tts))
            || !roles.iter().any(|r|r.purpose()==context.purpose) {
            return Err(RoutingError::Invalid("fixed voice requires a matching STT/TTS purpose").into());
        }
        tokio::select! {biased;
            _=cancel.notified()=>Err(RoutingError::Cancelled.into()),
            result=Box::pin(async {
                let policy_id=match &selection {ModelSelection::Auto {policy_id,..}=>*policy_id,_=>None};
                let policy=store::load_policy(state,&context.actor,policy_id).await?;
                engine::validate_policy(&policy)?;
                let mut constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints);
                for role in roles {
                    let mut ctx=context.clone();ctx.purpose=role.purpose();
                    constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&ctx).await?);
                }
                let mut plan=ExecutionPlan {id:context.run_id,schema_version:SCHEMA_VERSION,actor_user_id:context.actor.user_id,
                    selection,decision:service::empty_decision(&context,&policy,"fixed_voice_pending"),
                    constraints,assignments:Default::default(),fixed_assignments:Default::default(), profile_snapshot:None,vnext_context:None };
                fixed_lease::pin(state,&context,&mut plan,roles).await?;
                let budget=RunBudget::new(plan.constraints.clone());
                Self::runtime(context,plan,budget,PipelineClient::from_state(state)?)
            })=>result,
        }
    }
    /// Transfer exclusive ownership from audio capture into text planning. Actor,
    /// chat, run ID, cancellation, original deadline and spent STT stay identical.
    /// On failure this runtime is consumed, preventing a caller from replaying a
    /// failed classifier against a fresh ledger or changing the selection.
    pub async fn select_voice(mut self,state:&crate::AppState,requirements:WorkloadRequirements,conversation:&str,
        fixed_roles:&[FixedRole],cancel:&Notify)->crate::Result<Self> {
        if Arc::strong_count(&self.0)!=1 || !self.0.budget.exclusive() || self.0.durable.is_some()
            || !self.plan().assignments.is_empty() || !self.plan().fixed_assignments.contains_key(&FixedRole::Stt)
            || self.0.children.load(std::sync::atomic::Ordering::SeqCst)!=0 {
            return Err(RoutingError::Incompatible("voice selection requires an exclusively owned pending audio runtime").into());
        }
        let mut context=self.context(CallPurpose::Primary);
        let ledger=service::PlanningLedger {policy_id:self.plan().decision.policy_id,policy_revision:self.plan().decision.policy_revision,budget:self.0.budget.clone()};
        let (plan,_)=self.drive(cancel,Box::pin(self.audio_access(async {
            self.validate_access().await?;
            let mut pinned=self.plan().clone();
            let prior_roles:Vec<_>=pinned.fixed_assignments.keys().copied().collect();
            fixed_lease::pin(state,&context,&mut pinned,&prior_roles).await?;
            let (mut plan,budget)=Box::pin(service::selected_plan_in(state,&context,self.plan().selection.clone(),requirements,conversation,&Notify::new(),Some(ledger))).await?;
            // Revalidate existing audio pins; never substitute a changed engine
            // between recognition and synthesis. Add only newly adopted helpers.
            plan.fixed_assignments=self.plan().fixed_assignments.clone();
            let roles:Vec<_>=plan.fixed_assignments.keys().copied().chain(fixed_roles.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect();
            fixed_lease::pin(state,&context,&mut plan,&roles).await?;
            plan.decision.reason_codes.push("voice_audio_ledger_carried".into());
            Ok((plan,budget))
        }))).await?;
        let inner=Arc::get_mut(&mut self.0).ok_or(RoutingError::Incompatible("voice runtime became shared during selection"))?;
        context.purpose=CallPurpose::Primary;inner.context=context;inner.plan=plan;
        self.check()?;Ok(self)
    }
    pub async fn transcribe(&self,state:&crate::AppState,audio:Vec<u8>,mime:&str,language:Option<&str>)->crate::Result<Value> {
        if audio.is_empty() || audio.len()>MAX_AUDIO_BYTES || mime.len()>128 || mime.parse::<reqwest::header::HeaderValue>().is_err()
            || language.is_some_and(|s|s.is_empty() || s.len()>35 || !s.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-')) {
            return Err(RoutingError::Invalid("invalid audio, MIME type or language").into());
        }
        match self.audio(state,Request::Transcribe {audio,mime:mime.into(),language:language.map(str::to_owned)}).await? {
            Response::Text(value)=>Ok(value),_=>unreachable!(),
        }
    }
    pub async fn speech(&self,state:&crate::AppState,text:&str,voice:Option<&str>)->crate::Result<AudioResponse> {
        Box::pin(self.speech_format(state,text,voice,false)).await
    }
    /// One fixed TTS call, requesting a self-describing container before accepting
    /// samples. A provider ignoring the format cannot feed compressed bytes to PCM.
    pub async fn speech_pcm(&self,state:&crate::AppState,text:&str,voice:Option<&str>)->crate::Result<crate::voice::telephony::routed_audio::PcmReply> {
        let audio=Box::pin(self.speech_format(state,text,voice,true)).await?;
        crate::voice::telephony::routed_audio::PcmReply::from_wav(&audio.bytes,&audio.mime)
    }
    /// Transport prerequisite only: callers still need an authenticated call
    /// witness. No telephone feature gate or Phone-origin guard is relaxed here.
    pub async fn speech_to_samples(&self,state:&crate::AppState,text:&str,voice:Option<&str>,sink:Arc<dyn crate::voice::VoiceSink>,cancel:&Notify)->crate::Result<()> {
        use crate::voice::{AudioDelivery,telephony::routed_audio::{Playback,SAMPLE_RATE}};
        if sink.wants()!=(AudioDelivery::Samples {rate:SAMPLE_RATE}) {
            return Err(RoutingError::Incompatible("routed PCM speech requires a 24 kHz sample sink").into());
        }
        let audio=self.drive(cancel,Box::pin(self.speech_pcm(state,text,voice))).await?;
        let mut playback=Playback::new(sink.clone());
        let result=self.drive(cancel,Box::pin(self.audio_access(async {
            let duration=Duration::from_millis(audio.samples().len() as u64*1000/(u64::from(SAMPLE_RATE)*2))+Duration::from_secs(5);
            tokio::time::timeout(duration,audio.deliver(sink.as_ref())).await
                .map_err(|_|crate::AppError::Unavailable("routed PCM playback stalled".into()))?;
            Ok(())
        }))).await;
        if result.is_ok() {playback.complete();} else {playback.clear().await;}
        result
    }
    async fn speech_format(&self,state:&crate::AppState,text:&str,voice:Option<&str>,wav:bool)->crate::Result<AudioResponse> {
        if text.trim().is_empty() || text.len()>MAX_SPEECH_TEXT_BYTES
            || voice.is_some_and(|s|s.is_empty() || s.len()>128 || s.chars().any(char::is_control)) {
            return Err(RoutingError::Invalid("invalid speech text or voice").into());
        }
        match self.audio(state,Request::Speech {text:text.into(),voice:voice.map(str::to_owned),wav}).await? {
            Response::Audio(value)=>Ok(value),_=>unreachable!(),
        }
    }
    async fn audio(&self,state:&crate::AppState,request:Request)->crate::Result<Response> {
        let role=match &request {Request::Transcribe {..}=>FixedRole::Stt,Request::Speech {..}=>FixedRole::Tts};
        // These bound provider HTTP bytes, including WAV/base64/multipart or JSON
        // expansion, not just client bytes. Expansion beyond the grant fails.
        let allowance=FixedAllowance {max_calls:1,input_bytes:if role==FixedRole::Stt {32*1024*1024} else {512*1024},
            output_bytes:if role==FixedRole::Stt {1024*1024} else {MAX_AUDIO_BYTES as u32}};
        self.guard(Box::pin(async {
            let _serial=if self.0.durable.is_some() {Some(self.0.serial.lock().await)} else {None};
            let context=self.context(role.purpose());
            let prepared=Box::pin(mixed::prepare(state,&context,self.plan(),&self.0.budget,&Default::default(),
                [(role,allowance)].into(),&Default::default(),Duration::from_secs(120))).await?;
            self.validate_access().await?;
            Box::pin(crate::workflows::sources::validate_run(state,self)).await?;
            if let Some(lease)=&self.0.durable {lease.checkpoint(&self.0.budget).await?;}
            let attempt=uuid::Uuid::now_v7();let lease=prepared.envelope.fixed_budget.as_ref().unwrap().id;
            self.audio_receipt(state,attempt,lease,role,"started",true,None).await?;
            let operation=async {
                let cancel=Notify::new();
                match request {
                    Request::Transcribe {audio,mime,language}=>self.0.client.transcribe(audio,&mime,language.as_deref(),prepared,&cancel).await.map(Response::Text),
                    Request::Speech {text,voice,wav}=>{
                        let mut body=serde_json::json!({"text":text,"voice":voice});
                        if wav {body["response_format"]=serde_json::json!("wav");}
                        self.0.client.speech(body,prepared,&cancel).await.map(Response::Audio)
                    },
                }
            };
            let result=self.audio_access(operation).await;
            if let Some(lease)=&self.0.durable {lease.checkpoint(&self.0.budget).await?;}
            let measured=match &result {
                Ok(Response::Text(v))=>{let c=&v["fixed_routing_report"]["calls"][0];c["input_bytes"].as_u64().zip(c["output_bytes"].as_u64())},
                Ok(Response::Audio(v))=>Some((v.input_bytes,v.bytes.len() as u64)),_=>None,
            };
            self.audio_receipt(state,attempt,lease,role,"finished",result.is_ok(),measured).await?;
            result
        })).await
    }
    async fn audio_access<T>(&self,future:impl Future<Output=crate::Result<T>>)->crate::Result<T> {
        self.validate_access().await?;
        tokio::pin!(future);
        let mut ticks=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(250),Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=ticks.tick()=>self.validate_access().await?,
            result=&mut future=>{self.validate_access().await?;return result;}
        }}
    }
    fn audio_receipt<'a>(&'a self,state:&'a crate::AppState,attempt:uuid::Uuid,lease:uuid::Uuid,role:FixedRole,phase:&'a str,success:bool,measured:Option<(u64,u64)>)
        ->std::pin::Pin<Box<dyn Future<Output=crate::Result<()>>+Send+'a>> {
        Box::pin(async move {
            let mut event=crate::audit::AuditEvent::action(format!("routing.audio.{phase}"),self.0.context.actor.role.as_str());
            event.actor_user_id=self.0.context.actor.user_id;event.resource_type=Some("routing_attempt".into());event.resource_id=Some(attempt);
            if !success {event.outcome=crate::audit::AuditOutcome::Failure;}
            event.model_agent_traceability=Some(serde_json::json!({"schema_version":1,"attempt_id":attempt,"run_id":self.plan().id,
                "lease_id":lease,"chat_id":self.0.context.chat_id,"call_id":self.phone_call(),"purpose":role.purpose(),"target":self.plan().fixed_assignments.get(&role),"phase":phase,
                "unsettled_without_finish":true,"automatic_replay":false}));
            event.token_usage=Some(serde_json::json!({"ledger_snapshot":self.usage(),"accounting":"included_in_run"}));
            event.payload=measured.map(|(input,output)|serde_json::json!({"provider_input_bytes":input,"provider_output_bytes":output}));
            Box::pin(crate::audit::append(&state.pg,&event)).await?;Ok(())
        })
    }
}
