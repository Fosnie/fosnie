// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Validated fixed-TTS output for the existing telephone PCM transport.
//! No provider selection, line authentication or implicit format conversion.
use std::{sync::Arc,time::Duration};
use crate::{Result,model_routing::RoutingError,voice::{AudioClip,VoiceSink}};

pub const SAMPLE_RATE:u32=24_000;
pub const MAX_SECONDS:usize=120;
const FRAME_BYTES:usize=SAMPLE_RATE as usize*2/50;

/// Construction proves the container and sample layout before any bytes play.
#[derive(Debug)]
pub struct PcmReply {samples:Vec<u8>}
impl PcmReply {
    pub fn from_wav(bytes:&[u8],mime:&str)->Result<Self> {
        let invalid=||RoutingError::Invalid("speech requires bounded PCM16 mono 24 kHz WAV");
        let mime=mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if !matches!(mime.as_str(),"audio/wav"|"audio/x-wav"|"audio/wave"|"audio/vnd.wave"|"application/octet-stream")
            || bytes.len()<12 || bytes.len()>crate::model_routing::run::MAX_AUDIO_BYTES
            || &bytes[..4]!=b"RIFF" || &bytes[8..12]!=b"WAVE" {return Err(invalid().into());}
        let u32_at=|at:usize|u32::from_le_bytes(bytes[at..at+4].try_into().unwrap());
        if u64::from(u32_at(4))+8!=bytes.len() as u64 {return Err(invalid().into());}
        let mut pos=12usize;let mut format=false;let mut data=None;
        while pos<bytes.len() {
            if bytes.len()-pos<8 {return Err(invalid().into());}
            let kind=&bytes[pos..pos+4];let size=u32_at(pos+4) as usize;
            let start=pos+8;let end=start.checked_add(size).ok_or_else(invalid)?;
            let padded=end.checked_add(size%2).ok_or_else(invalid)?;
            if padded>bytes.len() {return Err(invalid().into());}
            match kind {
                b"fmt "=>{
                    if format || data.is_some() || !(size==16 || (size==18 && &bytes[end-2..end]==[0,0])) {return Err(invalid().into());}
                    let u16_at=|at|u16::from_le_bytes(bytes[at..at+2].try_into().unwrap());
                    if u16_at(start)!=1 || u16_at(start+2)!=1 || u32_at(start+4)!=SAMPLE_RATE
                        || u32_at(start+8)!=SAMPLE_RATE*2 || u16_at(start+12)!=2 || u16_at(start+14)!=16 {
                        return Err(invalid().into());
                    }
                    format=true;
                },
                b"data"=>{
                    if !format || data.is_some() || size==0 || size%2!=0 || size>SAMPLE_RATE as usize*2*MAX_SECONDS {return Err(invalid().into());}
                    data=Some(start..end);
                },
                _=>(),
            }
            pos=padded;
        }
        let data=data.ok_or_else(invalid)?;Ok(Self {samples:bytes[data].to_vec()})
    }
    pub fn samples(&self)->&[u8] {&self.samples}
    pub(crate) async fn deliver(&self,sink:&dyn VoiceSink) {
        // Small aligned chunks let barge-in stop feeding the paced output promptly.
        for (seq,bytes) in self.samples.chunks(FRAME_BYTES).enumerate() {
            sink.audio(AudioClip {bytes:bytes.to_vec(),mime:"audio/pcm;rate=24000;channels=1".into(),seq:seq as u64}).await;
            tokio::task::yield_now().await;
        }
        sink.audio_end().await;
    }
}

/// Dropping a caller's future must also abandon samples already queued for playout.
pub(crate) struct Playback {sink:Arc<dyn VoiceSink>,armed:bool}
impl Playback {
    pub(crate) fn new(sink:Arc<dyn VoiceSink>)->Self {Self {sink,armed:true}}
    pub(crate) fn complete(&mut self) {self.armed=false;}
    pub(crate) async fn clear(&mut self) {
        let _=tokio::time::timeout(Duration::from_secs(5),self.sink.clear()).await;
        self.armed=false;
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        if self.armed {
            let sink=self.sink.clone();
            tokio::spawn(async move {let _=tokio::time::timeout(Duration::from_secs(5),sink.clear()).await;});
        }
    }
}
