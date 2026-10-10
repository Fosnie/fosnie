// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},future::Future,pin::Pin,time::Duration};
use base64::Engine;
use serde_json::Value;
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},model_routing::{ModelSelection,RoutingObjective},
    voice::{routed::Session,WebSocketSink},ws::protocol::ServerFrame};
async fn start(state:&AppState,actor:&AuthContext,chat:Option<Uuid>,selection:ModelSelection)->anyhow::Result<(Arc<Session>,mpsc::Receiver<ServerFrame>)> {
    let (tx,rx)=mpsc::channel(256);let state=state.clone();let actor=actor.clone();
    let session=tokio::spawn(async move {Session::start(state,actor,Arc::new(WebSocketSink::new(tx)),chat,None,None,selection,true).await}).await??;
    Ok((session,rx))
}
async fn utterance(session:&Session)->anyhow::Result<()> {
    let loud:Vec<u8>=(0..640).flat_map(|_|4000i16.to_le_bytes()).collect();
    let loud=base64::engine::general_purpose::STANDARD.encode(loud);
    let quiet=base64::engine::general_purpose::STANDARD.encode(vec![0u8;1280]);
    for _ in 0..8 {session.on_audio_chunk(loud.clone()).await?;}
    for _ in 0..45 {session.on_audio_chunk(quiet.clone()).await?;}Ok(())
}
async fn until(rx:&mut mpsc::Receiver<ServerFrame>,kind:&str)->anyhow::Result<Vec<Value>> {
    tokio::time::timeout(Duration::from_secs(12),async {
        let mut frames=vec![];
        while let Some(frame)=rx.recv().await {
            let value=serde_json::to_value(frame)?;let done=value["type"]==kind;
            if value["type"]=="voice.error" && kind!="voice.error" {anyhow::bail!("voice error: {value}");}
            frames.push(value);if done {return Ok(frames);}
        }
        anyhow::bail!("voice closed before {kind}")
    }).await?
}
async fn live_gate(state:&AppState,on:bool)->anyhow::Result<()> {
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('routing.voice_live_enabled',$1,'bool') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(if on {"true"} else {"false"}).execute(&state.pg).await?;Ok(())
}
async fn listening(rx:&mut mpsc::Receiver<ServerFrame>)->anyhow::Result<()> {
    loop {
        let frames=until(rx,"voice.state").await?;
        assert!(!frames.iter().any(|f|f["type"]=="voice.tts.chunk"));
        if frames.last().is_some_and(|f|f["state"]=="listening") {return Ok(());}
    }
}
pub fn check<'a>(state:&'a AppState,actor:&'a AuthContext,admin:&'a AuthContext,policy:Uuid,calls:&'a Arc<AtomicUsize>,mode:&'a Arc<AtomicUsize>,entered:&'a Arc<Notify>)
    ->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        eprintln!("routing integration: bounded browser voice sessions");
        use fosnie_backend::http::voice_routing::{set_live_enabled,Enabled};
        let auto=||ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy)};
        let before=calls.load(Ordering::SeqCst);
        assert!(start(state,actor,None,auto()).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        assert!(set_live_enabled(axum::extract::State(state.clone()),AuthUser(actor.clone()),axum::Json(Enabled {enabled:true})).await.is_err());
        let caps=set_live_enabled(axum::extract::State(state.clone()),AuthUser(admin.clone()),axum::Json(Enabled {enabled:true})).await?.0;
        assert_eq!(caps["scope"],"browser_pcm_batch_v1");assert_eq!(caps["telephony"],false);
        mode.store(9,Ordering::SeqCst);
        let (session,mut rx)=start(state,actor,None,auto()).await?;let chat=session.chat_id();
        assert!(start(state,actor,Some(chat),auto()).await.is_err(),"one voice session owns chat until cleanup");
        utterance(&session).await?;
        let frames=until(&mut rx,"voice.tts.end").await?;
        assert!(frames.iter().any(|f|f["type"]=="voice.final" && f["text"]=="Synthetic transcript"));
        assert!(frames.iter().any(|f|f["type"]=="chat.completed"));
        assert!(frames.iter().any(|f|f["type"]=="voice.tts.chunk" && f["audio_base64"]=="YXVkaW8="));
        assert_eq!(calls.load(Ordering::SeqCst)-before,4,"STT + judge + answer + speech share one budget");
        let usage:Value=sqlx::query_scalar("SELECT token_usage FROM audit_events WHERE action_type='voice.turn.routed' AND model_agent_traceability->>'chat_id'=$1 ORDER BY seq DESC LIMIT 1")
            .bind(chat.to_string()).fetch_one(&state.pg).await?;assert_eq!(usage["routing_usage"]["calls"],4);
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE chat_id=$1 AND role='assistant' AND completed_at IS NOT NULL AND interrupted_at IS NULL")
            .bind(chat).fetch_one(&state.pg).await?;assert_eq!(count,1);
        assert!(fosnie_backend::voice::routed::require_legacy_chat(state,Some(chat)).await.is_err());
        assert!(fosnie_backend::chat::regenerate_prepare(state,actor,chat,Uuid::now_v7(),None).await.is_err());
        // Manual barge-in during STT never starts the classifier or speaks audio.
        listening(&mut rx).await?;mode.store(10,Ordering::SeqCst);let before=calls.load(Ordering::SeqCst);
        utterance(&session).await?;tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;
        session.barge_in().await;listening(&mut rx).await?;
        assert_eq!(calls.load(Ordering::SeqCst),before+1);
        // Revocation while TTS is waiting suppresses the completed clip.
        mode.store(11,Ordering::SeqCst);utterance(&session).await?;
        tokio::time::timeout(Duration::from_secs(6),entered.notified()).await?;live_gate(state,false).await?;
        let frames=until(&mut rx,"voice.error").await?;assert!(!frames.iter().any(|f|f["type"]=="voice.tts.chunk"));
        session.shutdown().await;drop(session);drop(rx);live_gate(state,true).await?;
        // A saved source snapshot protects continuation even after session end.
        sqlx::query("UPDATE chats SET mode='research' WHERE id=$1").bind(chat).execute(&state.pg).await?;
        assert!(start(state,actor,Some(chat),auto()).await.is_err());
        // Disconnect during STT closes the request instead of detaching a reply.
        mode.store(10,Ordering::SeqCst);let (session,mut rx)=start(state,actor,None,ModelSelection::Default).await?;
        let before=calls.load(Ordering::SeqCst);utterance(&session).await?;
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;session.detach();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(calls.load(Ordering::SeqCst),before+1);while let Ok(frame)=rx.try_recv() {assert!(!matches!(frame,ServerFrame::VoiceTtsChunk {..}));}
        assert!(session.on_audio_chunk("AA==".into()).await.is_err());
        live_gate(state,false).await?;mode.store(0,Ordering::SeqCst);Ok(())
    })
}
