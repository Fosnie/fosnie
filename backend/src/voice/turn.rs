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

//! Turn detection — the latency lever. Three layers,
//! kept distinct: a cheap acoustic gate (VAD), STT endpointing, and a **semantic**
//! turn model (Smart-Turn) that predicts completeness from prosody and so can fire
//! before trailing silence — and, crucially, **holds** on a mid-thought pause. The
//! VAD + Smart-Turn run in an external sidecar (Silero + Smart-Turn v3); this is the
//! HTTP client to it plus the pure decision function. When the sidecar is absent the
//! decision degrades to the configured silence threshold alone.

use serde::Deserialize;

use crate::error::AppError;

/// The turn-detection signal for a recent audio window, from the sidecar.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnSignal {
    /// Speech is present right now (the barge-in input while the assistant speaks).
    #[serde(default)]
    pub is_speech: bool,
    /// The utterance has ended acoustically (an endpoint).
    #[serde(default)]
    pub endpoint: bool,
    /// The semantic model judges the speaker has finished their thought.
    #[serde(default)]
    pub turn_complete: bool,
    /// Confidence of the completeness judgement, ∈ [0,1].
    #[serde(default)]
    pub prob: f32,
}

/// Ask the turn-detection sidecar to classify a recent audio window (PCM16 mono,
/// base64 in the JSON body). An error (sidecar absent/unreachable) surfaces to the
/// caller, which then falls back to the silence-threshold gate.
pub async fn detect(
    http: &reqwest::Client,
    url: &str,
    pcm_window: &[u8],
    sample_rate: u32,
) -> Result<TurnSignal, AppError> {
    use base64::Engine;
    let body = serde_json::json!({
        "audio_base64": base64::engine::general_purpose::STANDARD.encode(pcm_window),
        "sample_rate": sample_rate,
    });
    let resp = http
        .post(format!("{}/detect", url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Unavailable(format!("turn detector connect: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Unavailable(format!("turn detector returned {}", resp.status())));
    }
    resp.json::<TurnSignal>()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("turn detector decode: {e}")))
}

/// Decide whether to end the speaker's turn. Acoustic/STT endpointing OR trailing
/// silence beyond the configured threshold ends the utterance; when a semantic
/// detector is present we additionally require it to judge the turn complete, so a
/// mid-thought pause holds. Without a detector the silence gate decides.
pub fn should_fire_turn(
    endpoint: bool,
    silence_ms: u64,
    threshold_ms: u64,
    detector_present: bool,
    turn_complete: bool,
) -> bool {
    let ended = endpoint || silence_ms >= threshold_ms;
    if detector_present {
        ended && turn_complete
    } else {
        ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_gate_fires_without_detector() {
        // Trailing silence past the threshold ends the turn.
        assert!(should_fire_turn(false, 700, 600, false, false));
        // ...but not before it.
        assert!(!should_fire_turn(false, 500, 600, false, false));
        // An STT endpoint ends it regardless of the silence timer.
        assert!(should_fire_turn(true, 0, 600, false, false));
    }

    #[test]
    fn detector_holds_a_midthought_pause() {
        // Silence elapsed but the speaker isn't done → HOLD (the whole point).
        assert!(!should_fire_turn(false, 700, 600, true, false));
        // Silence elapsed AND the semantic model agrees → fire.
        assert!(should_fire_turn(false, 700, 600, true, true));
        // The detector may fire BEFORE the silence threshold, once the utterance
        // has endpointed and it judges completeness.
        assert!(should_fire_turn(true, 100, 600, true, true));
        // ...but never fires while the utterance is still open (no endpoint, short
        // silence), even if a stale `turn_complete` is set.
        assert!(!should_fire_turn(false, 100, 600, true, true));
    }
}
