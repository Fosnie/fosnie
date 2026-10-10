// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::AuthContext,model_routing::{*,fixed::FixedRole,run::RoutedRun},
    voice::{VoiceSink,VoiceState,AudioClip,AudioDelivery,telephony::{TelephonySink,Wire,pace::{self,Outbound}}}};

#[derive(Default)]
struct BlockedSink {audio:AtomicUsize,ends:AtomicUsize,clears:AtomicUsize,entered:Notify,cleared:Notify}
#[async_trait::async_trait]
impl VoiceSink for BlockedSink {
    fn wants(&self)->AudioDelivery {AudioDelivery::Samples {rate:24_000}}
    async fn state(&self,_:VoiceState,_:bool) {}
    async fn partial(&self,_:&str) {}
    async fn transcript(&self,_:&str) {}
    async fn error(&self,_:&str) {}
    async fn audio(&self,clip:AudioClip) {
        assert_eq!(clip.bytes.len(),960);assert!(!clip.bytes.starts_with(b"RIFF"));
        self.audio.fetch_add(1,Ordering::SeqCst);self.entered.notify_one();std::future::pending::<()>().await;
    }
    async fn audio_end(&self) {self.ends.fetch_add(1,Ordering::SeqCst);}
    async fn clear(&self) {self.clears.fetch_add(1,Ordering::SeqCst);self.cleared.notify_one();}
}
async fn runtime(state:&AppState,actor:&AuthContext)->anyhow::Result<RoutedRun> {
    Ok(Box::pin(RoutedRun::fixed_voice(state,RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Tts},
        &[FixedRole::Tts],&Notify::new())).await?)
}
pub fn check<'a>(state:&'a AppState,actor:&'a AuthContext,calls:&'a Arc<AtomicUsize>,mode:&'a Arc<AtomicUsize>,entered:&'a Arc<Notify>)
    ->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        mode.store(12,Ordering::SeqCst);
        // Exercise both actual telephone encoders/pacers with local channels only.
        for wire in [Wire::Mulaw,Wire::Pcm16] {
            let run=runtime(state,actor).await?;let before=calls.load(Ordering::SeqCst);
            let (tx,mut rx)=mpsc::channel(32);let (pacer,task)=pace::spawn(tx,1);
            let (control,_control_rx)=mpsc::channel(8);
            let sink=Arc::new(TelephonySink::new(pacer,control,Default::default(),wire,false));
            Box::pin(run.speech_to_samples(state,"Synthetic read aloud",None,sink.clone(),&Notify::new())).await?;
            let mut frames=0;while let Ok(item)=rx.try_recv() {if let Outbound::Frame(bytes)=item {
                frames+=1;assert_eq!(bytes.len(),if wire==Wire::Mulaw {160} else {320});
                assert!(bytes.iter().all(|v|*v==if wire==Wire::Mulaw {255} else {0}));
            }}
            assert_eq!(frames,3,"50 ms of input pads to three 20 ms wire frames");
            assert_eq!(calls.load(Ordering::SeqCst),before+1);assert_eq!(run.usage().calls,1);
            drop(sink);tokio::time::timeout(Duration::from_secs(2),task).await??;
        }
        // Refuse unsupported sinks before paying for synthesis.
        let run=Box::pin(RoutedRun::fixed_voice(state,RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Stt},
            &[FixedRole::Stt,FixedRole::Tts],&Notify::new())).await?;
        Box::pin(run.transcribe(state,fosnie_backend::voice::stt_stream::pcm_to_wav(&[0,0],16000),"audio/wav",None)).await?;
        assert_eq!(Box::pin(run.speech_pcm(state,"Synthetic read aloud",None)).await?.samples().len(),2400);
        assert_eq!(run.usage().calls,2);let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(run.speech_pcm(state,"Synthetic read aloud",None)).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),before,"PCM conversion cannot create a new inference budget");
        // Refuse unsupported sinks before paying for synthesis.
        let run=runtime(state,actor).await?;let before=calls.load(Ordering::SeqCst);
        let (tx,_rx)=mpsc::channel(8);
        assert!(Box::pin(run.speech_to_samples(state,"Synthetic read aloud",None,Arc::new(fosnie_backend::voice::WebSocketSink::new(tx)),&Notify::new())).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),before);assert_eq!(run.usage().calls,0);
        // Unsupported encoding/rate and unproven provider reports never play.
        for fault in [13,14,15] {
            mode.store(fault,Ordering::SeqCst);let run=runtime(state,actor).await?;let sink=Arc::new(BlockedSink::default());
            assert!(Box::pin(run.speech_to_samples(state,"Synthetic read aloud",None,sink.clone(),&Notify::new())).await.is_err());
            assert_eq!(sink.audio.load(Ordering::SeqCst),0);assert_eq!(sink.ends.load(Ordering::SeqCst),0);
            assert_eq!(run.usage().calls,1,"bad audio never refunds inference");
        }
        mode.store(12,Ordering::SeqCst);
        // Cancellation and dropped futures clear already queued samples and never
        // emit the completion marker; the TTS call remains spent in both cases.
        for drop_task in [false,true] {
            let run=runtime(state,actor).await?;let sink=Arc::new(BlockedSink::default());
            let pending=tokio::spawn({let state=state.clone();let run=run.clone();let sink=sink.clone();async move {
                run.speech_to_samples(&state,"Synthetic read aloud",None,sink,&Notify::new()).await
            }});
            tokio::time::timeout(Duration::from_secs(3),sink.entered.notified()).await?;
            if drop_task {pending.abort();assert!(pending.await.unwrap_err().is_cancelled());}
            else {run.cancel();assert!(tokio::time::timeout(Duration::from_secs(2),pending).await??.is_err());}
            tokio::time::timeout(Duration::from_secs(2),sink.cleared.notified()).await?;
            assert_eq!(sink.audio.load(Ordering::SeqCst),1);assert_eq!(sink.ends.load(Ordering::SeqCst),0);
            assert_eq!(sink.clears.load(Ordering::SeqCst),1);assert_eq!(run.usage().calls,1);
        }
        // A synthesis cancelled before any bytes are returned never reaches PCM.
        mode.store(16,Ordering::SeqCst);let run=runtime(state,actor).await?;let sink=Arc::new(BlockedSink::default());
        let pending=tokio::spawn({let state=state.clone();let run=run.clone();let sink=sink.clone();async move {
            run.speech_to_samples(&state,"Synthetic read aloud",None,sink,&Notify::new()).await
        }});
        tokio::time::timeout(Duration::from_secs(3),entered.notified()).await?;run.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(2),pending).await??.is_err());
        assert_eq!(sink.audio.load(Ordering::SeqCst),0);assert_eq!(run.usage().calls,1);
        mode.store(0,Ordering::SeqCst);Ok(())
    })
}
