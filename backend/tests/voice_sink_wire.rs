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

//! The browser transport, checked against the wire.
//!
//! A live-voice session no longer builds the frames it sends: it reports what
//! happened and the transport decides how to say it. That is a good arrangement
//! only for as long as the browser transport says exactly what the session used to
//! say, so each event is driven once and the bytes are compared with the very same
//! snapshots the protocol crate pins.

use fosnie_backend::voice::{AudioClip, VoiceSink, VoiceState, WebSocketSink};
use fosnie_backend::ws::protocol::ServerFrame;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Everything queued so far, as the bytes the socket would carry.
fn drain(rx: &mut mpsc::Receiver<ServerFrame>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(f) = rx.try_recv() {
        out.push(f.to_json());
    }
    out
}

#[tokio::test]
async fn each_event_becomes_the_frame_it_always_was() {
    let (tx, mut rx) = mpsc::channel(32);
    let sink = WebSocketSink::new(tx);

    sink.state(VoiceState::Listening, false).await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_state.json")]
    );

    sink.state(VoiceState::Listening, true).await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_state_retrieving.json")]
    );

    sink.partial("how much rain").await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_partial.json")]
    );

    sink.transcript("how much rain fell?").await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_final.json")]
    );

    // The audio crosses the boundary as bytes, and this is where they are encoded.
    // The three bytes below are `AAEC` in the padded standard alphabet and nothing
    // else: an encoder that dropped the padding, or used the alphabet meant for
    // URLs, would compile perfectly and hand the browser audio it cannot decode.
    sink.audio(AudioClip { bytes: vec![0, 1, 2], mime: "audio/mpeg".into(), seq: 7 }).await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_tts_chunk.json")]
    );

    sink.audio_end().await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_tts_end.json")]
    );

    sink.error("the assistant timed out").await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_error.json")]
    );
}

/// A mirrored chat frame goes out untouched. It is the turn's own frame, and the
/// client reads it exactly as it reads one from a typed conversation.
#[tokio::test]
async fn a_mirrored_chat_frame_is_passed_along_unchanged() {
    let (tx, mut rx) = mpsc::channel(4);
    let sink = WebSocketSink::new(tx);
    let turn_id = Uuid::from_bytes([3; 16]);

    sink.relay(ServerFrame::ChatToken { turn_id, delta: "Hello".into() }).await;
    assert_eq!(
        drain(&mut rx),
        vec![include_str!("../../crates/fosnie-protocol/tests/fixtures/chat_token.json")]
    );
}

/// The browser holds no queue of its own on this side of the socket, so there is
/// nothing here to discard: a cut clause is never sent at all. Were this to start
/// emitting something, every interruption would gain a frame no client expects.
#[tokio::test]
async fn discarding_queued_speech_says_nothing_to_a_browser() {
    let (tx, mut rx) = mpsc::channel(4);
    let sink = WebSocketSink::new(tx);
    sink.clear().await;
    assert!(drain(&mut rx).is_empty(), "clearing must be silent on this transport");
}

/// Voice events and mirrored chat frames share one channel, so the order they are
/// reported in is the order the client sees. Two channels would be faster to write
/// and would let a citation arrive before the answer it belongs to.
#[tokio::test]
async fn voice_and_chat_frames_keep_the_order_they_were_reported_in() {
    let (tx, mut rx) = mpsc::channel(16);
    let sink = WebSocketSink::new(tx);
    let turn_id = Uuid::from_bytes([3; 16]);

    sink.state(VoiceState::Speaking, false).await;
    sink.relay(ServerFrame::ChatToken { turn_id, delta: "one".into() }).await;
    sink.audio(AudioClip { bytes: vec![0], mime: "audio/mpeg".into(), seq: 0 }).await;
    sink.relay(ServerFrame::ChatToken { turn_id, delta: "two".into() }).await;
    sink.audio_end().await;

    let tags: Vec<String> = drain(&mut rx)
        .iter()
        .map(|json| {
            let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
            v["type"].as_str().expect("a tag").to_string()
        })
        .collect();
    assert_eq!(
        tags,
        vec!["voice.state", "chat.token", "voice.tts.chunk", "chat.token", "voice.tts.end"]
    );
}
