// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Streaming-STT client. Opens a persistent WebSocket
//! to an external streaming-STT engine (sherpa-onnx online-websocket-server / NeMo),
//! forwards PCM16 frames, and yields incremental `partial` + `final` transcripts —
//! the batch OpenAI-audio contract cannot emit partials. When no streaming engine
//! is configured/reachable the orchestrator falls back to per-utterance **batch**
//! transcription (`ml::transcribe` over `pcm_to_wav`), so live voice still works on
//! a box that only has the batch engine — at the cost of no live partials.
//!
//! Rust-direct to the engine (not via the Python ML service): the live loop is on
//! the latency-critical path and owns cancel, so a Python hop is avoided.

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::error::AppError;

/// One transcript event from the streaming engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    /// A live, still-stabilising hypothesis (shown muted).
    Partial { text: String },
    /// A settled segment of the utterance.
    Final { text: String },
    /// A transient engine error (the session may continue).
    Error { message: String },
}

/// The engine's wire shape: `{"type":"partial"|"final"|"error","text":..,"message":..}`.
#[derive(Deserialize)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    message: Option<String>,
}

/// A live streaming-STT session. Push PCM16 mono frames with [`send_pcm`]; pull
/// transcript events with [`recv`]. Dropping it aborts both tasks → the engine
/// socket closes (this is how the orchestrator cancels STT on teardown/barge-in).
///
/// [`send_pcm`]: SttStream::send_pcm
/// [`recv`]: SttStream::recv
pub struct SttStream {
    pcm_tx: mpsc::Sender<Vec<u8>>,
    /// End-of-utterance signal. The sherpa writer ignores it (the engine segments via
    /// its own VAD); the OpenAI-realtime writer turns it into `input_audio_buffer.commit`.
    commit_tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<SttEvent>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    /// Whether this engine needs an explicit [`commit`](Self::commit) to produce a
    /// transcript (OpenAI realtime with `turn_detection:null`). sherpa = false.
    manual_commit: bool,
}

impl SttStream {
    /// Assemble a session from its already-spawned reader/writer tasks + channels.
    /// `pub(crate)` so the per-engine adapters (e.g. `stt_openai_realtime`) construct it.
    pub(crate) fn new(
        pcm_tx: mpsc::Sender<Vec<u8>>,
        commit_tx: mpsc::Sender<()>,
        rx: mpsc::Receiver<SttEvent>,
        reader: JoinHandle<()>,
        writer: JoinHandle<()>,
        manual_commit: bool,
    ) -> Self {
        Self { pcm_tx, commit_tx, rx, reader, writer, manual_commit }
    }

    /// Forward one PCM16 frame to the engine. Best-effort (a closed engine drops it).
    pub async fn send_pcm(&self, pcm: Vec<u8>) {
        let _ = self.pcm_tx.send(pcm).await;
    }

    /// Signal end-of-utterance (the orchestrator's Smart-Turn fired). A no-op for
    /// engines that self-segment; required before OpenAI realtime emits a transcript.
    pub async fn commit(&self) {
        let _ = self.commit_tx.send(()).await;
    }

    /// Whether the orchestrator must call [`commit`](Self::commit) (and wait for the
    /// final) to get a transcript, vs the engine emitting finals on its own.
    pub fn manual_commit(&self) -> bool {
        self.manual_commit
    }

    /// Await the next transcript event, or `None` once the engine stream ends.
    pub async fn recv(&mut self) -> Option<SttEvent> {
        self.rx.recv().await
    }
}

impl Drop for SttStream {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

/// Open a streaming-STT WebSocket session. Errors only on connect/handshake; once
/// open, transient engine errors arrive as [`SttEvent::Error`]. `ws://` only (no
/// TLS feature) — the engine sits on the in-perimeter LAN.
pub async fn open(url: &str, _sample_rate: u32) -> Result<SttStream, AppError> {
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| AppError::Unavailable(format!("streaming STT connect: {e}")))?;
    let (mut sink, mut stream) = ws.split();

    let (pcm_tx, mut pcm_rx) = mpsc::channel::<Vec<u8>>(64);
    let (commit_tx, mut commit_rx) = mpsc::channel::<()>(8);
    let (ev_tx, ev_rx) = mpsc::channel::<SttEvent>(64);

    // Writer: forward captured PCM to the engine as binary frames; close on drain.
    // `commit` is a no-op here — sherpa/NeMo segment via their own VAD — but we still
    // drain the channel so the orchestrator's `commit()` never blocks.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                pcm = pcm_rx.recv() => match pcm {
                    Some(pcm) => {
                        if sink.send(Message::Binary(pcm.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = commit_rx.recv() => { /* sherpa self-segments — ignore */ }
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Reader: parse JSON transcript frames → SttEvent.
    let reader = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { break };
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => match String::from_utf8(b.to_vec()) {
                    Ok(s) => s,
                    Err(_) => continue,
                },
                Message::Close(_) => break,
                _ => continue,
            };
            if text.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<WireEvent>(&text) {
                let out = match ev.kind.as_str() {
                    "partial" => SttEvent::Partial { text: ev.text },
                    "final" => SttEvent::Final { text: ev.text },
                    "error" => SttEvent::Error { message: ev.message.unwrap_or_default() },
                    _ => continue,
                };
                if ev_tx.send(out).await.is_err() {
                    break;
                }
            }
        }
    });

    Ok(SttStream::new(pcm_tx, commit_tx, ev_rx, reader, writer, false))
}

/// Wrap raw PCM16 mono little-endian samples in a minimal RIFF/WAVE header, so the
/// batch fallback can POST a self-describing WAV to the existing `/transcribe`.
pub fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let block_align = channels * (bits / 8);
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt-chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_to_wav_has_riff_header_and_lengths() {
        let pcm = vec![0u8; 320]; // 10 ms @ 16 kHz mono 16-bit
        let wav = pcm_to_wav(&pcm, 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + pcm.len());
        // Sample rate (offset 24) and data length (offset 40), both LE u32.
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 16_000);
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), pcm.len() as u32);
        // RIFF chunk size = 36 + data.
        assert_eq!(u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]), 36 + pcm.len() as u32);
    }
}
