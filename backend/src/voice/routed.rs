// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Bounded PCM sessions: acoustic endpoint -> fixed STT -> routed chat -> fixed TTS.
//! Phone sessions require an authenticated call witness and use validated PCM output.
use std::{future::Future,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering}},time::Duration};
use base64::Engine;
use serde_json::{json,Value};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use crate::{AppState,Result,AppError,auth::AuthContext,model_routing::{*,fixed::FixedRole,run::{RoutedRun,AccessGuard}},ws::protocol::ServerFrame};
use super::{VoiceSink,VoiceState,VoiceKnobs,VoiceProfile,AudioClip};

pub const GATE:&str="routing.voice_live_enabled";
const RATE:u32=16_000;
const MAX_PCM:usize=RATE as usize*2*30;
pub async fn enabled(state:&AppState)->Result<bool> {
    use crate::config::runtime::{self,ConfigValueType};
    Ok(runtime::get(&state.pg,GATE).await?.is_some_and(|v|matches!(v.value_type,ConfigValueType::Bool) && v.value=="true"))
}
pub async fn description(state:&AppState)->Result<Value> {
    Ok(json!({"schema_version":1,"enabled":enabled(state).await?,"scope":"browser_pcm_batch_v1","start_frame":"voice.routed.start",
        "selection_modes":["default","manual","auto"],"sample_rate":RATE,"pcm":"s16le_mono","max_utterance_seconds":30,
        "endpoint":"acoustic_silence","stt":"bounded_batch","tts":"completed_answer_clip",
        "realtime_stt":false,"speculative_retrieval":false,"telephony":false,"automatic_replay":false,
        "legacy_history_adoption":false}))
}
pub async fn require_legacy_chat(state:&AppState,chat:Option<Uuid>)->Result<()> {
    crate::model_routing::admission::require_legacy_chat(state,chat).await
}
pub(crate) async fn validate_chat(state:&AppState,run:&RoutedRun)->Result<()> {
    let context=run.context(CallPurpose::Primary);let Some(chat)=context.chat_id else {return Ok(());};
    let prior:Option<Value>=sqlx::query_scalar("SELECT source_scope FROM voice_routed_chats WHERE chat_id=$1").bind(chat).fetch_optional(&state.pg).await?;
    if let Some(call)=run.phone_call() {
        if prior.as_ref().is_none_or(|v|v["call_id"]!=json!(call)) {return Err(RoutingError::Forbidden.into());}
        return run.validate_access().await;
    }
    if let Some(prior)=prior {if capture(state,&context.actor,chat).await?!=prior {return Err(RoutingError::Forbidden.into());}}
    Ok(())
}
pub(crate) fn capture<'a>(state:&'a AppState,actor:&'a AuthContext,chat:Uuid)->std::pin::Pin<Box<dyn Future<Output=Result<Value>>+Send+'a>> {
    capture_text(state,actor,chat,crate::chat::origin::ChatOrigin::Web)
}
/// Shared remote-text source snapshot. Voice still calls the Web-only wrapper.
pub(crate) fn capture_text<'a>(state:&'a AppState,actor:&'a AuthContext,chat:Uuid,origin:crate::chat::origin::ChatOrigin)->std::pin::Pin<Box<dyn Future<Output=Result<Value>>+Send+'a>> {
    Box::pin(async move {
    if !matches!(origin,crate::chat::origin::ChatOrigin::Web|crate::chat::origin::ChatOrigin::Desktop) {return Err(RoutingError::Forbidden.into());}
    let row:Option<(Option<Uuid>,Option<Uuid>,String)>=sqlx::query_as("SELECT project_id,agent_id,mode FROM chats WHERE id=$1 AND owner_user_id=$2 AND origin=$3 AND archived_at IS NULL")
        .bind(chat).bind(actor.user_id).bind(origin.as_str()).fetch_optional(&state.pg).await?;
    let (project,agent,mode)=row.ok_or(RoutingError::Forbidden)?;
    if mode!="general" {return Err(RoutingError::Incompatible("remote routing requires general mode").into());}
    let workspace:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_workspace WHERE chat_id=$1)").bind(chat).fetch_one(&state.pg).await?;
    if workspace {return Err(RoutingError::Incompatible("remote routing does not adopt a device workspace").into());}
    if let Some(project)=project {state.rbac.require_project(&state.pg,actor,project,crate::auth::rbac::Permission::Read).await?;}
    let config=if let Some(agent)=agent {
        let config=crate::workflows::agents::capture(&mut *state.pg.acquire().await?,agent).await?;
        if config.is_null() || (!actor.is_admin() && !config["created_by"].is_null() && config["created_by"]!=json!(actor.user_id)) {
            return Err(RoutingError::Forbidden.into());
        }
        if !config["modes"].as_array().is_some_and(|v|v.iter().any(|v|v=="general")) {return Err(RoutingError::Forbidden.into());}
        let configured:Vec<Uuid>=serde_json::from_value(config["kb_ids"].clone()).map_err(|_|RoutingError::Forbidden)?;
        for id in configured {crate::kb::require_read(&state.pg,actor,id).await?;}config
    } else {Value::Null};
    let mut libraries=crate::kb::retrieval_allowlist(&state.pg,actor,chat,project,agent).await?;libraries.sort();libraries.dedup();
    for id in &libraries {crate::kb::require_read(&state.pg,actor,*id).await?;crate::kb::brief(&state.pg,*id).await?;}
    let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,actor,&libraries).await?;denied.sort();denied.dedup();
    Ok(json!({"project":project,"agent_id":agent,"agent":config,"libraries":libraries,"denied":denied}))
    })
}
#[derive(Default)]
struct Stop {stopped:AtomicBool,notify:Arc<Notify>,id:Uuid}
impl Stop {
    fn cancel(&self) {self.stopped.store(true,Ordering::SeqCst);self.notify.notify_waiters();self.notify.notify_one();}
    fn check(&self)->Result<()> {if self.stopped.load(Ordering::SeqCst) {Err(RoutingError::Cancelled.into())} else {Ok(())}}
}
/// A stalled browser must not retain a database session lock indefinitely.
struct Output {inner:Arc<dyn VoiceSink>,stop:Arc<Stop>}
impl Output {
    async fn send(&self,future:impl Future<Output=()>) {
        if tokio::time::timeout(Duration::from_secs(5),future).await.is_err() {self.stop.cancel();}
    }
}
#[async_trait::async_trait]
impl VoiceSink for Output {
    fn wants(&self)->super::AudioDelivery {self.inner.wants()}
    async fn state(&self,state:VoiceState,retrieving:bool) {self.send(self.inner.state(state,retrieving)).await;}
    async fn partial(&self,text:&str) {self.send(self.inner.partial(text)).await;}
    async fn transcript(&self,text:&str) {self.send(self.inner.transcript(text)).await;}
    async fn audio(&self,clip:AudioClip) {self.send(self.inner.audio(clip)).await;}
    async fn audio_end(&self) {
        if matches!(self.wants(),super::AudioDelivery::Samples {..}) {self.inner.audio_end().await;}
        else {self.send(self.inner.audio_end()).await;}
    }
    async fn error(&self,message:&str) {self.send(self.inner.error(message)).await;}
    async fn relay(&self,frame:ServerFrame) {self.send(self.inner.relay(frame)).await;}
    async fn clear(&self) {self.send(self.inner.clear()).await;}
}
struct Scope {state:AppState,actor:AuthContext,chat:Uuid,snapshot:Value,session:Arc<Stop>,turn:Arc<Stop>,phone:Option<Arc<crate::telephony::routing::Binding>>}
#[async_trait::async_trait]
impl AccessGuard for Scope {
    fn phone_call(&self)->Option<Uuid> {self.phone.as_ref().map(|p|p.call)}
    async fn check(&self)->Result<()> {
        self.session.check()?;self.turn.check()?;
        let current=if let Some(phone)=&self.phone {phone.capture(&self.state,&self.actor,Some(self.chat)).await?}
            else {principal(&self.state,&self.actor).await?;capture(&self.state,&self.actor,self.chat).await?};
        if current!=self.snapshot {return Err(RoutingError::Forbidden.into());}Ok(())
    }
}
impl Scope {
    async fn guard<T>(&self,future:impl Future<Output=Result<T>>)->Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval(Duration::from_millis(250));ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=self.session.notify.notified()=>return Err(RoutingError::Cancelled.into()),
            _=self.turn.notify.notified()=>return Err(RoutingError::Cancelled.into()),
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
}
async fn principal(state:&AppState,actor:&AuthContext)->Result<()> {
    let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND role::text=$2 AND deactivated_at IS NULL)")
        .bind(actor.user_id).bind(actor.role.as_str()).fetch_one(&state.pg).await?;
    if !live || actor.break_glass || actor.mfa_enroll_only || !state.features.enabled_for(state,actor,"voice").await
        || !state.features.enabled_for(state,actor,"voice_live").await {return Err(RoutingError::Forbidden.into());}
    if !enabled(state).await? {return Err(AppError::Unavailable("routed live voice is disabled".into()));}Ok(())
}

pub struct Session {
    state:AppState,actor:AuthContext,chat:Uuid,selection:ModelSelection,snapshot:Value,sink:Arc<dyn VoiceSink>,
    stop:Arc<Stop>,current:Mutex<Option<Arc<Stop>>>,epoch:AtomicU64,pcm:mpsc::Sender<Vec<u8>>,
    finished:AtomicBool,done:Notify,
    phone:Option<Arc<crate::telephony::routing::Binding>>,phone_plan:Mutex<Option<ExecutionPlan>>,
    opening:Mutex<Option<RoutedRun>>,notice:AtomicBool,opening_started:AtomicBool,
}
impl Session {
    /// Only browser-authenticated callers may use this constructor. Phone/device
    /// authentication must not be silently converted into a Web conversation.
    pub fn start(state:AppState,actor:AuthContext,sink:Arc<dyn VoiceSink>,chat:Option<Uuid>,project:Option<Uuid>,agent:Option<Uuid>,
        selection:ModelSelection,aec:bool)->std::pin::Pin<Box<dyn Future<Output=Result<Arc<Self>>>+Send>> {
        Box::pin(async move {
        principal(&state,&actor).await?;
        if sink.wants()!=super::AudioDelivery::Clip {return Err(RoutingError::Incompatible("routed live voice requires a browser clip transport").into());}
        if let Some(project)=project {state.rbac.require_project(&state.pg,&actor,project,crate::auth::rbac::Permission::Read).await?;}
        if let Some(agent)=agent {
            let config=crate::workflows::agents::capture(&mut *state.pg.acquire().await?,agent).await?;
            if config.is_null() || (!actor.is_admin() && !config["created_by"].is_null() && config["created_by"]!=json!(actor.user_id))
                || !config["modes"].as_array().is_some_and(|v|v.iter().any(|v|v=="general")) {return Err(RoutingError::Forbidden.into());}
        }
        crate::cache::rate_limit_guard(&state.redis,&format!("voicelive:{}",actor.user_id.unwrap_or_default()),10,60).await?;
        let created=chat.is_none();let chat=match chat {
            Some(chat)=>chat,
            None=>{
                let chat=Uuid::now_v7();
                sqlx::query("INSERT INTO chats(id,owner_user_id,title,project_id,agent_id,origin) VALUES($1,$2,'Voice chat',$3,$4,'web')")
                    .bind(chat).bind(actor.user_id).bind(project).bind(agent).execute(&state.pg).await?;chat
            }
        };
        let mut lease=state.pg.begin().await?;
        let locked:bool=sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("voice-chat:{chat}")).fetch_one(&mut *lease).await?;
        if !locked {return Err(AppError::Conflict("another voice session owns this conversation".into()));}
        let snapshot=capture(&state,&actor,chat).await?;
        if project.is_some_and(|id|snapshot["project"]!=json!(id)) || agent.is_some_and(|id|snapshot["agent_id"]!=json!(id)) {
            return Err(RoutingError::Forbidden.into());
        }
        Box::pin(crate::chat::routing::workflow_scope(&state,&actor,chat,"")).await?;
        let saved:Option<Value>=sqlx::query_scalar("SELECT source_scope FROM voice_routed_chats WHERE chat_id=$1").bind(chat).fetch_optional(&state.pg).await?;
        match saved {
            Some(saved) if saved==snapshot=>(),
            None if created=>{sqlx::query("INSERT INTO voice_routed_chats(chat_id,source_scope) VALUES($1,$2)").bind(chat).bind(&snapshot).execute(&state.pg).await?;},
            _=>return Err(AppError::Conflict("start a new voice conversation; history or source scope cannot be adopted".into())),
        }
        sqlx::query("UPDATE chats SET model_selection=$2 WHERE id=$1").bind(chat).bind(json!(selection)).execute(&state.pg).await?;
        let knobs=VoiceKnobs::load_for(&state.pg,VoiceProfile::Browser).await;
        let (tx,rx)=mpsc::channel(256);
        let stop=Arc::new(Stop::default());let sink=Arc::new(Output {inner:sink,stop:stop.clone()});
        let session=Arc::new(Self {state,actor,chat,selection,snapshot,sink,stop,current:Mutex::new(None),epoch:AtomicU64::new(0),pcm:tx,finished:AtomicBool::new(false),done:Notify::new(),
            phone:None,phone_plan:Mutex::new(None),opening:Mutex::new(None),notice:AtomicBool::new(false),opening_started:AtomicBool::new(false)});
        if created {session.sink.relay(ServerFrame::ChatCreated {chat_id:chat}).await;}
        session.sink.state(VoiceState::Listening,false).await;
        session.stop.check()?;
        let me=session.clone();tokio::spawn(async move {me.capture_loop(rx,knobs,aec,lease).await;});Ok(session)
        })
    }
    pub(crate) fn start_phone(state:AppState,actor:AuthContext,sink:Arc<dyn VoiceSink>,phone:Arc<crate::telephony::routing::Binding>)
        ->std::pin::Pin<Box<dyn Future<Output=Result<Arc<Self>>>+Send>> {
        Box::pin(async move {
            if sink.wants()!=(super::AudioDelivery::Samples {rate:24000}) {return Err(RoutingError::Forbidden.into());}
            let mut lease=state.pg.begin().await?;
            let locked:bool=sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("routed-phone:{}",phone.call)).fetch_one(&mut *lease).await?;
            if !locked {return Err(AppError::Conflict("another session owns this call".into()));}
            let before=phone.capture(&state,&actor,None).await?;
            let selection=phone.details.routing.as_ref().ok_or(RoutingError::Forbidden)?.model_selection.clone();
            let chat=Uuid::now_v7();let mut create=state.pg.begin().await?;
            sqlx::query("INSERT INTO chats(id,owner_user_id,agent_id,title,origin,model_selection) VALUES($1,$2,$3,'Phone call','phone',$4)")
                .bind(chat).bind(actor.user_id).bind(phone.details.agent_id).bind(json!(selection)).execute(&mut *create).await?;
            let n=sqlx::query("UPDATE calls SET chat_id=$2 WHERE id=$1 AND chat_id IS NULL AND ended_at IS NULL AND outcome='in_progress'")
                .bind(phone.call).bind(chat).execute(&mut *create).await?.rows_affected();
            if n!=1 {return Err(RoutingError::Forbidden.into());}create.commit().await?;
            let snapshot=phone.capture(&state,&actor,Some(chat)).await?;
            if snapshot!=before {return Err(RoutingError::Forbidden.into());}
            Box::pin(crate::chat::routing::workflow_scope(&state,&actor,chat,"")).await?;
            sqlx::query("INSERT INTO voice_routed_chats(chat_id,source_scope) VALUES($1,$2)").bind(chat).bind(&snapshot).execute(&state.pg).await?;
            let knobs=VoiceKnobs::load_for(&state.pg,VoiceProfile::Phone).await;
            let (tx,rx)=mpsc::channel(256);let stop=Arc::new(Stop::default());
            let sink=Arc::new(Output {inner:sink,stop:stop.clone()});
            let session=Arc::new(Self {state,actor,chat,selection,snapshot,sink,stop,current:Mutex::new(None),epoch:AtomicU64::new(0),pcm:tx,
                finished:AtomicBool::new(false),done:Notify::new(),phone:Some(phone),phone_plan:Mutex::new(None),opening:Mutex::new(None),notice:AtomicBool::new(true),opening_started:AtomicBool::new(false)});
            let scope=session.scope(Arc::new(Stop {id:Uuid::now_v7(),..Default::default()}));
            let context=RoutingContext {actor:session.actor.clone(),chat_id:Some(chat),project_id:None,run_id:scope.turn.id,purpose:CallPurpose::Stt};
            let run=scope.guard(Box::pin(RoutedRun::pending_voice(&session.state,context,session.selection.clone(),&[FixedRole::Stt,FixedRole::Tts],&scope.turn.notify))).await?.with_access(scope.clone())?;
            *session.phone_plan.lock().unwrap()=Some(run.plan().clone());*session.opening.lock().unwrap()=Some(run);
            *session.current.lock().unwrap()=Some(scope.turn.clone());
            let me=session.clone();tokio::spawn(async move {me.capture_loop(rx,knobs,true,lease).await;});Ok(session)
        })
    }
    pub(crate) async fn announce_phone(&self,text:String)->bool {
        let Some(phone)=&self.phone else {return false;};
        if self.opening_started.swap(true,Ordering::SeqCst) || text!=phone.details.opening || text.trim().is_empty() {return false;}
        let run=self.opening.lock().unwrap().take();let Some(run)=run else {return false;};
        let result=Box::pin(run.speech_to_samples(&self.state,&text,None,self.sink.clone(),&self.stop.notify)).await;
        if result.is_err() || self.stop.check().is_err() {self.detach();return false;}
        let mut event=crate::audit::AuditEvent::action("voice.notice.routed",self.actor.role.as_str());
        event.actor_user_id=self.actor.user_id;event.resource_type=Some("call".into());event.resource_id=Some(phone.call);
        event.model_agent_traceability=Some(json!({"run_id":run.plan().id,"chat_id":self.chat,"call_id":phone.call,"status":"completed"}));
        event.token_usage=Some(json!({"routing_usage":run.usage()}));
        if Box::pin(crate::audit::append(&self.state.pg,&event)).await.is_err() {self.detach();return false;}
        *self.current.lock().unwrap()=None;self.notice.store(false,Ordering::SeqCst);true
    }
    pub fn chat_id(&self)->Uuid {self.chat}
    pub async fn on_audio_chunk(&self,audio:String)->Result<()> {
        self.stop.check()?;
        if audio.len()>5500 {self.detach();return Err(RoutingError::Invalid("voice PCM frame too large").into());}
        let pcm=match base64::engine::general_purpose::STANDARD.decode(audio) {Ok(pcm)=>pcm,Err(_)=>{self.detach();return Err(RoutingError::Invalid("invalid voice PCM").into());}};
        self.on_pcm(pcm).await
    }
    pub(crate) async fn on_pcm(&self,pcm:Vec<u8>)->Result<()> {
        self.stop.check()?;
        if self.notice.load(Ordering::SeqCst) {return Ok(());}
        if pcm.is_empty() || pcm.len()>4096 || pcm.len()%2!=0 {self.detach();return Err(RoutingError::Invalid("invalid voice PCM frame size").into());}
        if self.pcm.try_send(pcm).is_err() {self.detach();return Err(AppError::Unavailable("voice capture queue overflow".into()));}Ok(())
    }
    pub async fn barge_in(&self) {
        if self.notice.load(Ordering::SeqCst) {return;}
        if let Some(stop)=self.current.lock().unwrap().as_ref() {stop.cancel();}
        self.epoch.fetch_add(1,Ordering::SeqCst);self.sink.clear().await;
        self.sink.state(VoiceState::Interrupted,false).await;
    }
    pub async fn cancel_turn(&self,id:Uuid) {
        let matches=self.current.lock().unwrap().as_ref().is_some_and(|s|s.id==id);
        if matches {self.barge_in().await;}
    }
    pub fn detach(&self) {
        self.stop.cancel();if let Some(stop)=self.current.lock().unwrap().as_ref() {stop.cancel();}
    }
    pub async fn shutdown(&self) {
        self.detach();self.sink.clear().await;
        loop {
            let notified=self.done.notified();tokio::pin!(notified);notified.as_mut().enable();
            if self.finished.load(Ordering::SeqCst) {break;}notified.await;
        }
    }
    async fn capture_loop(self:Arc<Self>,mut rx:mpsc::Receiver<Vec<u8>>,knobs:VoiceKnobs,aec:bool,lease:sqlx::Transaction<'static,sqlx::Postgres>) {
        let mut work:Option<tokio::task::JoinHandle<()>>=None;
        let mut audio=Vec::new();let mut speech=0u64;let mut silence=0u64;let mut barge=0u64;let mut epoch=0;
        let mut ticks=tokio::time::interval(Duration::from_millis(250));ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {biased;
                _=self.stop.notify.notified()=>break,
                _=ticks.tick()=>{
                    let scope=self.scope(Arc::new(Stop::default()));
                    if scope.check().await.is_err() {self.sink.error("Voice session access changed or routing was disabled").await;break;}
                },
                _=async {match work.as_mut() {Some(task)=>{let _=task.await;},None=>std::future::pending::<()>().await}}=>{
                    work=None;*self.current.lock().unwrap()=None;barge=0;
                    self.sink.state(VoiceState::Listening,false).await;
                },
                pcm=rx.recv()=>{
                    let Some(pcm)=pcm else {break;};
                    if self.notice.load(Ordering::SeqCst) {continue;}
                    let current_epoch=self.epoch.load(Ordering::SeqCst);
                    if current_epoch!=epoch {audio.clear();speech=0;silence=0;epoch=current_epoch;}
                    let ms=(pcm.len() as u64*1000)/(RATE as u64*2);
                    let energy=rms(&pcm);
                    if work.is_some() {
                        barge=if aec && energy>=knobs.barge_rms {barge.saturating_add(ms)} else {0};
                        if barge>=knobs.barge_min_ms {self.barge_in().await;barge=0;}continue;
                    }
                    if energy>=knobs.speech_rms {speech=speech.saturating_add(ms);silence=0;}
                    else if speech>0 {silence=silence.saturating_add(ms);} else {continue;}
                    if audio.len()+pcm.len()>MAX_PCM {audio.clear();speech=0;silence=0;self.sink.error("Voice utterance exceeds 30 seconds").await;continue;}
                    audio.extend_from_slice(&pcm);
                    if silence<knobs.silence_threshold_ms {continue;}
                    if speech<knobs.min_speech_ms {audio.clear();speech=0;silence=0;continue;}
                    let pcm=std::mem::take(&mut audio);speech=0;silence=0;
                    let stop=Arc::new(Stop {id:Uuid::now_v7(),..Default::default()});*self.current.lock().unwrap()=Some(stop.clone());
                    let me=self.clone();work=Some(tokio::spawn(async move {me.run_utterance(pcm,stop).await;}));
                }
            }
        }
        self.detach();if let Some(work)=work {let _=work.await;}
        if let Some(phone)=&self.phone {if phone.ending.requested().is_none() {phone.ending.end_now(crate::telephony::log::CallEnd::Dropped);}}
        self.sink.clear().await;
        let _=lease.rollback().await;self.finished.store(true,Ordering::SeqCst);self.done.notify_waiters();
    }
    fn scope(&self,turn:Arc<Stop>)->Arc<Scope> {Arc::new(Scope {state:self.state.clone(),actor:self.actor.clone(),chat:self.chat,
        snapshot:self.snapshot.clone(),session:self.stop.clone(),turn,phone:self.phone.clone()})}
    async fn run_utterance(&self,pcm:Vec<u8>,stop:Arc<Stop>) {
        let id=stop.id;let scope=self.scope(stop);
        let result=Box::pin(self.execute(id,pcm,&scope)).await;
        if let Err(error)=result {
            let mut event=crate::audit::AuditEvent::action("voice.turn.routed",self.actor.role.as_str());
            event.actor_user_id=self.actor.user_id;event.resource_type=Some("voice_turn".into());event.resource_id=Some(id);
            event.outcome=crate::audit::AuditOutcome::Failure;
            event.model_agent_traceability=Some(json!({"run_id":id,"chat_id":self.chat,"status":"failed_or_cancelled","accounting":"see_correlated_attempt_receipts"}));
            let _=Box::pin(crate::audit::append(&self.state.pg,&event)).await;
            if scope.turn.check().is_ok() && scope.session.check().is_ok() {
                tracing::warn!(%error,"routed voice turn failed");self.sink.error("Voice turn failed; no automatic retry was made").await;
                if self.phone.is_some() {self.detach();}
            }
        }
    }
    async fn execute(&self,id:Uuid,pcm:Vec<u8>,scope:&Arc<Scope>)->Result<()> {
        scope.check().await?;
        crate::cache::rate_limit_guard(&self.state.redis,&format!("voice:{}",self.actor.user_id.unwrap_or_default()),40,60).await?;
        self.sink.state(VoiceState::Thinking,false).await;
        let project=serde_json::from_value(self.snapshot["project"].clone()).map_err(|_|RoutingError::Forbidden)?;
        let context=RoutingContext {actor:self.actor.clone(),chat_id:Some(self.chat),project_id:project,run_id:id,purpose:CallPurpose::Stt};
        let run=scope.guard(Box::pin(RoutedRun::pending_voice(&self.state,context,self.selection.clone(),&[FixedRole::Stt,FixedRole::Tts],&scope.turn.notify))).await?
            .with_access(scope.clone())?;
        if let Some(pinned)=self.phone_plan.lock().unwrap().as_ref() {
            if run.plan().decision.policy_id!=pinned.decision.policy_id || run.plan().decision.policy_revision!=pinned.decision.policy_revision
                || run.plan().fixed_assignments!=pinned.fixed_assignments {return Err(RoutingError::NeedsReplan.into());}
        }
        let transcript=scope.guard(Box::pin(run.transcribe(&self.state,super::stt_stream::pcm_to_wav(&pcm,RATE),"audio/wav",None))).await?;
        let text=transcript["text"].as_str().ok_or(RoutingError::Invalid("missing transcript"))?.trim().to_owned();
        if text.is_empty() {return Err(RoutingError::Invalid("no speech recognized").into());}
        Box::pin(crate::chat::routing::workflow_scope(&self.state,&self.actor,self.chat,&text)).await?;
        let run=scope.guard(Box::pin(crate::chat::routing::prepare_voice(&self.state,run,&text,WorkloadRequirements {output_tokens:4096,..Default::default()},&[],&scope.turn.notify))).await?;
        scope.check().await?;self.sink.transcript(&text).await;
        let (tx,mut rx)=mpsc::channel(256);
        let chat=async {
            let turn=if let Some(phone)=&self.phone {crate::chat::origin::TurnContext::new(&self.actor,crate::chat::origin::ChatOrigin::Phone).with_call(Some(phone.call))}
                else {crate::chat::origin::TurnContext::web(&self.actor)};
            Box::pin(crate::chat::run_turn(&self.state,turn.with_routing(&run),id,Some(self.chat),None,None,
                text,vec![],vec![],false,None,None,None,None,&tx,scope.turn.notify.clone())).await;drop(tx);
        };
        let drain=async {
            let mut answer=String::new();let mut complete=false;
            loop {
                let frame=match scope.guard(async {Ok(rx.recv().await)}).await {Ok(frame)=>frame,Err(error)=>{run.cancel();rx.close();return Err(error);}};
                let Some(frame)=frame else {break;};
                match &frame {
                    ServerFrame::ChatToken {delta,..}=>{
                        if answer.len()+delta.len()>crate::model_routing::run::MAX_SPEECH_TEXT_BYTES {run.cancel();rx.close();return Err(RoutingError::BudgetExhausted.into());}answer.push_str(delta);
                    },
                    ServerFrame::ChatCompleted {..}=>complete=true,
                    ServerFrame::ChatInterrupted {..}|ServerFrame::ChatError {..}|ServerFrame::AgentApproval {..}=>{run.cancel();},
                    _=>(),
                }
                if let Err(error)=run.guard(scope.guard(async {self.sink.relay(frame).await;Ok(())})).await {run.cancel();rx.close();return Err(error);}
            }
            if !complete || answer.trim().is_empty() {return Err(AppError::Unavailable("voice answer did not complete".into()));}Ok(answer)
        };
        let (_,answer)=tokio::join!(Box::pin(chat),Box::pin(drain));
        run.finish_children().await?;let answer=answer?;scope.check().await?;
        self.sink.state(VoiceState::Speaking,false).await;
        let audio=if self.phone.is_some() {
            scope.guard(Box::pin(run.speech_to_samples(&self.state,&answer,None,self.sink.clone(),&scope.turn.notify))).await?;None
        } else {Some(scope.guard(Box::pin(run.speech(&self.state,&answer,None))).await?)};
        let mut event=crate::audit::AuditEvent::action("voice.turn.routed",self.actor.role.as_str());
        event.actor_user_id=self.actor.user_id;event.resource_type=Some("voice_turn".into());event.resource_id=Some(id);
        event.model_agent_traceability=Some(json!({"run_id":id,"chat_id":self.chat,"decision":run.plan().decision,"selection":run.plan().selection,
            "target":run.plan().assignments.get(&CallPurpose::Final),"call_id":self.phone.as_ref().map(|p|p.call),"status":"completed"}));
        event.token_usage=Some(json!({"routing_usage":run.usage()}));Box::pin(crate::audit::append(&self.state.pg,&event)).await?;
        if let Some(audio)=audio {
            scope.check().await?;self.sink.audio(AudioClip {bytes:audio.bytes,mime:audio.mime,seq:0}).await;
            scope.check().await?;self.sink.audio_end().await;
        }
        scope.check().await?;Ok(())
    }
}
fn rms(pcm:&[u8])->f64 {
    let sum:f64=pcm.chunks_exact(2).map(|v|{let n=i16::from_le_bytes([v[0],v[1]]) as f64/32768.0;n*n}).sum();
    (sum/(pcm.len()/2).max(1) as f64).sqrt()
}
