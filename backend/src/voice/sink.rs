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

//! Where a live-voice session's output goes.
//!
//! The orchestrator above this file is transport-agnostic: it detects turns, runs
//! the chat turn, aggregates clauses and synthesises them. What differs between a
//! browser tab and a telephone line is only the last hop, so that hop is a trait
//! and the session holds one.
//!
//! Events cross the boundary as what they *are*, not as frames. Audio in
//! particular crosses as raw bytes plus its media type, because base64 and the
//! frame envelope are facts about a WebSocket and nothing else: a transport that
//! wants 8 kHz samples on a socket would otherwise have to undo both to find the
//! audio it needs.
//!
//! [`WebSocketSink`] is the browser transport, and it holds exactly **one**
//! sender. Both the voice events and the mirrored chat-turn frames go through it,
//! so the order they are emitted in is the order they reach the client. Splitting
//! them across two channels would let a citation overtake the token it belongs to.

use async_trait::async_trait;
use base64::Engine as _;
use tokio::sync::mpsc;

use super::VoiceState;
use crate::ws::protocol::ServerFrame;

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// What shape a transport needs synthesised audio in.
///
/// The two are not interchangeable and neither is a preference. A recipient with a
/// decoder wants whole, self-contained clips and would gain nothing from raw
/// samples; a recipient that has to resample and pace cannot open a container at
/// all, and would gain nothing from a clip it cannot take apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDelivery {
    /// One complete clip per clause, in whatever format the synthesiser produced,
    /// labelled with its own media type.
    Clip,
    /// Raw signed 16-bit little-endian mono samples at `rate`, handed over as they
    /// are synthesised rather than gathered up.
    Samples { rate: u32 },
}

/// One synthesised clause, as the engine produced it.
pub struct AudioClip {
    pub bytes: Vec<u8>,
    /// The media type of `bytes`, as the synthesiser labelled it.
    pub mime: String,
    /// Position in this turn's reply, from zero.
    pub seq: u64,
}

/// The far end of a live-voice session.
#[async_trait]
pub trait VoiceSink: Send + Sync {
    /// The conversation moved to `state`; `retrieving` is whether a speculative
    /// search is running alongside it.
    async fn state(&self, state: VoiceState, retrieving: bool);

    /// Interim recognition of what the speaker is currently saying.
    async fn partial(&self, text: &str);

    /// The settled transcript of a finished utterance.
    async fn transcript(&self, text: &str);

    /// Speak one clause of the reply.
    async fn audio(&self, clip: AudioClip);

    /// The reply is fully spoken.
    async fn audio_end(&self);

    /// Something went wrong, in words the speaker can be told.
    async fn error(&self, message: &str);

    /// What shape this transport needs the reply audio in.
    ///
    /// Deliberately not async: it is a fixed property of the transport, and an
    /// answer that could be awaited is an answer that could change halfway through
    /// a sentence.
    fn wants(&self) -> AudioDelivery {
        AudioDelivery::Clip
    }

    /// Mirror one frame from the underlying chat turn: the transcript text,
    /// citations, tool steps, the persistence notices. Purely for whatever is
    /// watching the conversation, so the default is to drop it. A transport with
    /// nobody watching loses nothing by doing so: everything the session itself
    /// needs from the turn is taken out of these frames before they get here.
    async fn relay(&self, _frame: ServerFrame) {}

    /// The clock of the reply now starting.
    ///
    /// A transport that can observe its far end stamps the moments only it can know: when
    /// the speaker began hearing the reply, and when they finished. Defaulted away because
    /// most transports cannot see that far, and deliberately not async: it is a handover,
    /// not an operation.
    fn reply_clock(&self, _clock: std::sync::Arc<crate::voice::session::TurnClock>) {}

    /// Discard speech already handed over but not yet heard.
    ///
    /// Called when the speaker talks over the reply. A transport that plays audio
    /// the instant it arrives has nothing buffered to discard, hence the default;
    /// one that hands whole clauses to something with a playout queue of its own
    /// has to empty that queue, or the interruption is not audible for as long as
    /// the queue is deep.
    async fn clear(&self) {}
}

/// The browser transport: every event becomes the frame the client already knows.
pub struct WebSocketSink {
    /// The one socket sender. See the note at the top of this file: one channel is
    /// load-bearing, not incidental.
    tx: mpsc::Sender<ServerFrame>,
}

impl WebSocketSink {
    pub fn new(tx: mpsc::Sender<ServerFrame>) -> Self {
        Self { tx }
    }

    /// Queue one frame. Awaits when the socket is behind, which is the back
    /// pressure that keeps the turn from outrunning the client: dropping instead
    /// would lose tokens and audio silently under load.
    async fn send(&self, frame: ServerFrame) {
        let _ = self.tx.send(frame).await;
    }
}

#[async_trait]
impl VoiceSink for WebSocketSink {
    async fn state(&self, state: VoiceState, retrieving: bool) {
        self.send(ServerFrame::VoiceLiveState { state: state.as_str().into(), retrieving }).await;
    }

    async fn partial(&self, text: &str) {
        self.send(ServerFrame::VoicePartial { text: text.to_string() }).await;
    }

    async fn transcript(&self, text: &str) {
        self.send(ServerFrame::VoiceFinal { text: text.to_string() }).await;
    }

    async fn audio(&self, clip: AudioClip) {
        self.send(ServerFrame::VoiceTtsChunk {
            audio_base64: B64.encode(&clip.bytes),
            mime: clip.mime,
            seq: clip.seq,
        })
        .await;
    }

    async fn audio_end(&self) {
        self.send(ServerFrame::VoiceTtsEnd).await;
    }

    async fn error(&self, message: &str) {
        self.send(ServerFrame::VoiceError { message: message.to_string() }).await;
    }

    async fn relay(&self, frame: ServerFrame) {
        self.send(frame).await;
    }

    // `clear` stays the default no-op. The browser holds no server-side playout
    // queue: a cut clause is never sent at all, and the player stops the audio it
    // has already scheduled the moment it hears the reply was interrupted.

    // `wants` stays the default `Clip`. The browser has a decoder and the frame it
    // reads carries its own media type, so a complete clip per clause is both what
    // it wants and what it has always been sent.
}
