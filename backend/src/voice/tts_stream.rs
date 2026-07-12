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

//! Streaming-TTS client. One [`TtsStream`] per clause:
//! audio chunks arrive as the engine synthesises them, so playback can begin on the
//! first chunk rather than waiting for the whole clause. Two backends behind one
//! type — a chunked OpenAI-audio engine (kokoro-fastapi `stream=true`) and a batch
//! fallback that wraps `ml::synthesize` as a single-chunk stream. Because the
//! [`SentenceAggregator`](super::aggregate) already splits the LLM output at clause
//! boundaries, even the batch fallback yields fast first-audio.
//!
//! Dropping a [`TtsStream`] aborts its reader → the engine response is dropped →
//! synthesis stops. That is how barge-in cuts audio cleanly mid-clause.

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::AppError;

/// Build the `/v1/audio/speech` URL with exactly one `/v1` segment. Operators set
/// the engine base inconsistently — local kokoro as `http://localhost:8880` (no
/// `/v1`), cloud OpenAI as `https://api.openai.com/v1` (matching the chat roles).
/// Without this, `…/v1` + `/v1/audio/speech` doubles to `…/v1/v1/…` → 404.
fn audio_speech_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{}/v1/audio/speech", base.trim_end_matches('/'))
}

/// Does the base point at OpenAI? (Empty voice ⇒ a valid OpenAI voice, since
/// OpenAI rejects the local engines' `default`/`af_sky`.)
fn is_openai(base_url: &str) -> bool {
    base_url.contains("api.openai.com")
}

/// A streaming-TTS session for one clause.
pub struct TtsStream {
    rx: mpsc::Receiver<Vec<u8>>,
    reader: JoinHandle<()>,
    /// MIME of the audio chunks (e.g. `audio/mpeg`), for the `voice.tts.chunk` frame.
    pub mime: String,
}

impl TtsStream {
    /// Await the next audio chunk, or `None` once the clause is fully synthesised.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

impl Drop for TtsStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Stream one clause from a chunked OpenAI-audio `/v1/audio/speech` engine. `http`
/// should be a client WITHOUT the ML shared-secret header (the engine is reached
/// directly, not via the ML service). `api_key` is `None` for a local engine
/// (kokoro-fastapi, no auth) and `Some(..)` for cloud OpenAI (`Authorization: Bearer`).
pub async fn stream_clause(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    text: &str,
    voice: Option<&str>,
    api_key: Option<&str>,
) -> Result<TtsStream, AppError> {
    let url = audio_speech_url(base_url);
    let openai = is_openai(base_url);
    // Empty voice → a valid default for the engine: OpenAI rejects `default`, so use
    // `alloy` there; local engines (kokoro) accept `default`.
    let voice = voice.filter(|v| !v.is_empty()).unwrap_or(if openai { "alloy" } else { "default" });
    let mut body = serde_json::json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": "mp3",
    });
    // `stream: true` is a kokoro-fastapi extension for chunked synthesis. OpenAI's
    // /v1/audio/speech has no such field and returns a payload the browser can't
    // play as audio/mpeg — it streams the mp3 body natively, so omit it there.
    if !openai {
        body["stream"] = serde_json::Value::Bool(true);
    }
    let mut req = http.post(url.as_str()).json(&body);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Unavailable(format!("streaming TTS connect: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Unavailable(format!("streaming TTS returned {}", resp.status())));
    }
    // We always request mp3, so present the stream as audio/mpeg regardless of what
    // the provider labels it — the player decodes mp3 clips and some engines mislabel
    // or omit the Content-Type.
    let mime = "audio/mpeg".to_string();
    let (tx, rx) = mpsc::channel::<Vec<u8>>(32);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            if bytes.is_empty() {
                continue;
            }
            if tx.send(bytes.to_vec()).await.is_err() {
                return; // receiver dropped (barge-in / teardown) → stop reading
            }
        }
    });
    Ok(TtsStream { rx, reader, mime })
}

/// Batch-synthesise one clause via the platform ML service (`ml::synthesize`),
/// wrapped as a single-chunk [`TtsStream`] so the orchestrator path is uniform.
pub async fn batch_clause(
    http: &reqwest::Client,
    ml_base_url: &str,
    text: &str,
    voice: Option<&str>,
    providers: crate::ml::ProviderOverrides,
) -> Result<TtsStream, AppError> {
    let (bytes, mime) = crate::ml::synthesize(http, ml_base_url, text, voice, providers).await?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
    let reader = tokio::spawn(async move {
        let _ = tx.send(bytes).await;
    });
    Ok(TtsStream { rx, reader, mime })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_speech_url_has_exactly_one_v1() {
        // Cloud base already carries /v1 — must not double.
        assert_eq!(audio_speech_url("https://api.openai.com/v1"), "https://api.openai.com/v1/audio/speech");
        assert_eq!(audio_speech_url("https://api.openai.com/v1/"), "https://api.openai.com/v1/audio/speech");
        // Local engine base without /v1.
        assert_eq!(audio_speech_url("http://localhost:8880"), "http://localhost:8880/v1/audio/speech");
        assert_eq!(audio_speech_url("http://localhost:8880/"), "http://localhost:8880/v1/audio/speech");
    }

    #[test]
    fn is_openai_detects_host() {
        assert!(is_openai("https://api.openai.com/v1"));
        assert!(!is_openai("http://localhost:8880"));
    }
}
