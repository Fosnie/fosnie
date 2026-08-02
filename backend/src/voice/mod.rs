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

//! Live / streaming voice (some mode-3 aspects are deferred). A per-socket orchestrator
//! couples the existing WebSocket transport, the existing chat-turn, and cancel:
//!
//! ```text
//! client PCM ─▶ streaming STT ─▶ partials+finals ─▶ (endpoint ∧ Smart-Turn)
//!            ─▶ final transcript ─▶ chat::run_turn (LLM token stream)
//!            ─▶ SentenceAggregator (clauses) ─▶ streaming TTS ─▶ audio chunks
//!   ‖ barge-in monitor runs throughout ‖
//! ```
//!
//! Every engine is an **external, in-perimeter, swappable** service; any absent
//! engine **degrades** (batch STT per utterance / silence-threshold gate / batch
//! TTS per clause) so the loop still runs. The orchestrator lives in Rust because
//! it is transport + turn-taking + cancel, all of which Rust already owns; the LLM
//! stage reuses `chat::run_turn` verbatim (the live turn persists like any chat).

pub mod aggregate;
pub mod dictation;
pub mod session;
pub mod sink;
pub mod spec_retrieval;
pub mod stt_openai_realtime;
pub mod stt_stream;
pub mod telephony;
pub mod tts_stream;
pub mod turn;

pub use dictation::DictationSession;
pub use session::Session;
pub use sink::{AudioClip, AudioDelivery, VoiceSink, WebSocketSink};

/// The conversation state surfaced to the SPA (`voice.state`). Distinct visuals per
/// state are mandatory for a professional voice UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceState {
    Idle,
    Listening,
    Capturing,
    Thinking,
    Speaking,
    Interrupted,
    Error,
}

impl VoiceState {
    pub fn as_str(self) -> &'static str {
        match self {
            VoiceState::Idle => "idle",
            VoiceState::Listening => "listening",
            VoiceState::Capturing => "capturing",
            VoiceState::Thinking => "thinking",
            VoiceState::Speaking => "speaking",
            VoiceState::Interrupted => "interrupted",
            VoiceState::Error => "error",
        }
    }
}

/// Which transport a session is tuned for.
///
/// A telephone line is not a quieter browser. It is narrowband, it has no screen to show
/// that anything is happening, it has no button to hold, and its echo cancellation is
/// somebody else's, upstream. Several dials want different values there, and tuning one
/// transport must not move the other: that is the whole reason this exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceProfile {
    Browser,
    Phone,
}

impl VoiceProfile {
    /// The key a dial is tried under before the shared `voice.<dial>` one.
    ///
    /// `None` means this transport has no profile of its own, so resolution is exactly
    /// what it has always been. That is what keeps the browser path untouched: not a
    /// promise, a returned `None`.
    fn override_key(self, dial: &str) -> Option<String> {
        match self {
            VoiceProfile::Browser => None,
            VoiceProfile::Phone => Some(format!("voice.phone.{dial}")),
        }
    }

    /// The transport as a metric label. A fixed string per variant, so a label can never
    /// be built from anything measured at runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            VoiceProfile::Browser => "browser",
            VoiceProfile::Phone => "phone",
        }
    }
}

/// The runtime-tunable dials for the live-voice loop, read fresh per session from
/// the super-admin knob store (mirrors `ml::rag_overrides`). Defaults match the
/// knob registry in `http::superadmin`.
#[derive(Debug, Clone)]
pub struct VoiceKnobs {
    /// Trailing-silence (ms) before the speaker's turn is ended (the latency lever).
    pub silence_threshold_ms: u64,
    /// Minimum total *speech* (ms above the RMS gate) in an utterance before it fires a
    /// turn. Below this it's noise/a blip — discarded without transcription, so a
    /// near-silent clip can't be hallucinated into text. Low enough to keep short words.
    pub min_speech_ms: u64,
    /// Default to push-to-talk rather than an open VAD-gated mic.
    pub ptt_default: bool,
    /// Require browser echo cancellation before honouring barge-in.
    pub aec_required: bool,
    /// Consult the turn-detection sidecar (else the silence threshold alone decides).
    pub turn_detection: bool,
    /// Normalised loudness (0..1) at or above which a frame counts as speech at all.
    ///
    /// **A placeholder awaiting measurement on a telephone line.** The value below was
    /// chosen for a browser microphone with echo cancellation applied. A line arrives
    /// band-limited to roughly 300 to 3400 Hz and resampled, which strips energy a
    /// microphone captures, while its companding and level planning aim at a much higher
    /// nominal level than a microphone at default gain. The two pull in opposite
    /// directions and nobody has measured which wins, so the phone profile ships the same
    /// value rather than an invented one. `voice_frame_rms` is the instrument: read the
    /// capture and talk-over distributions off it and set the phone dials from what is
    /// there.
    pub speech_rms: f64,
    /// Normalised loudness at or above which a frame counts as talking over the reply.
    /// Higher than [`VoiceKnobs::speech_rms`], and the same placeholder caveat applies.
    pub barge_rms: f64,
    /// How much continuous talk-over interrupts the reply. A quiet frame resets the run,
    /// so a spike never reaches it.
    pub barge_min_ms: u64,
    /// Start the knowledge-base search from the partial transcript, while the
    /// speaker is still talking, so its cost falls outside the reply budget.
    pub spec_enabled: bool,
    /// Minimum words in the query before speculating.
    pub spec_min_words: u64,
    /// Minimum growth since the previous speculative search.
    pub spec_min_new_words: u64,
    /// Minimum gap between speculative searches.
    pub spec_debounce_ms: u64,
    /// Cap on speculative searches per utterance.
    pub spec_max_fires: u64,
    /// Soft endpoint as a percentage of `silence_threshold_ms`: the pause at which
    /// the turn is probably ending, and the transcript is worth searching on, but
    /// not yet long enough to end it.
    pub spec_soft_silence_pct: u64,
    /// Turn-completeness probability that also counts as a soft endpoint. Needs the
    /// turn-detection sidecar; `1.0` leaves the silence threshold in sole charge.
    pub spec_eager_prob: f32,
    /// Deadline for a speculative search (far tighter than a turn's own retrieval:
    /// a speculation that has not landed by the time the speaker stops is worthless).
    pub spec_timeout_secs: u64,
    /// Reuse gate: token-Jaccard similarity at or above which a speculative result
    /// answers the committed transcript.
    pub spec_reuse_jaccard: f32,
    /// Reuse gate: when the speculative query is a word-prefix of the committed
    /// transcript, the largest fraction of it that may be words never searched for.
    pub spec_reuse_new_ratio: f32,
}

impl Default for VoiceKnobs {
    fn default() -> Self {
        Self {
            // Without a semantic turn detector the silence gate alone ends a turn, so
            // keep it generous — a natural mid-thought pause must not chop the speaker
            // (especially in hands-free). The Smart-Turn sidecar can fire sooner.
            silence_threshold_ms: 1500,
            min_speech_ms: 200,
            ptt_default: true,
            aec_required: true,
            turn_detection: false,
            speech_rms: 0.012,
            // Talking over the assistant needs a louder, sustained signal than plain
            // capture: the assistant's own audio echoing back into an open microphone is
            // quieter than direct speech, and one spike must never cut the reply.
            barge_rms: 0.035,
            barge_min_ms: 320,
            spec_enabled: true,
            spec_min_words: 5,
            spec_min_new_words: 4,
            spec_debounce_ms: 700,
            spec_max_fires: 3,
            spec_soft_silence_pct: 50,
            spec_eager_prob: 0.4,
            spec_timeout_secs: 12,
            // Starting points, to be calibrated from the per-turn counters on a live
            // deployment rather than guessed at.
            spec_reuse_jaccard: 0.7,
            spec_reuse_new_ratio: 0.35,
        }
    }
}

/// The dial rows as they stand, read in one go.
///
/// Read together rather than one key at a time because the dials are resolved once per
/// session and there are a lot of them: a dial per query is a round trip per dial, on
/// the path that answers a telephone.
struct DialRows {
    rows: std::collections::HashMap<String, String>,
    profile: VoiceProfile,
}

impl DialRows {
    async fn load(pg: &sqlx::PgPool, profile: VoiceProfile) -> Self {
        // Runtime-typed rather than a checked macro: the shape is two text columns and
        // nothing about it can drift, so it costs no offline-query churn. The pattern
        // takes the profile rows along with the shared ones, so a transport with its own
        // profile still costs one round trip.
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM config_settings WHERE key LIKE 'voice.%'")
                .fetch_all(pg)
                .await
                .unwrap_or_default();
        DialRows { rows: rows.into_iter().collect(), profile }
    }

    /// What is set for `dial` on this transport: its own row if there is one, otherwise
    /// the shared one.
    ///
    /// So a value set for everybody reaches a telephone too, and only a value set
    /// explicitly for the telephone overrides it. Worth knowing before wondering why a
    /// line followed a change made for the browser.
    ///
    /// An absent row and an unreachable store are the same answer, which is what the
    /// per-key reads did too: an unset dial keeps its default.
    fn raw(&self, dial: &str) -> Option<&str> {
        if let Some(key) = self.profile.override_key(dial) {
            if let Some(value) = self.rows.get(&key) {
                return Some(value.as_str());
            }
        }
        self.rows.get(&format!("voice.{dial}")).map(String::as_str)
    }

    /// A set row means the dial is on only if it says so. Anything else set means off,
    /// and only an absent row falls back.
    fn get_bool(&self, key: &str, dflt: bool) -> bool {
        self.raw(key).map(|v| v == "true").unwrap_or(dflt)
    }

    /// A whole number. Something set but unreadable falls back, rather than becoming
    /// zero and silently opening a gate.
    fn get_u64(&self, key: &str, dflt: u64) -> u64 {
        self.raw(key).and_then(|v| v.parse::<u64>().ok()).unwrap_or(dflt)
    }

    /// Fractional dials, clamped here. The knob store enforces the declared range for
    /// whole numbers only, so a fraction arrives unvalidated and a mistyped one would
    /// otherwise disable or wildly loosen a gate.
    fn get_f32(&self, key: &str, dflt: f32) -> f32 {
        self.raw(key)
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(dflt)
            .clamp(0.0, 1.0)
    }

    /// The loudness gates, which are normalised to 0..1 and so clamp the same way.
    fn get_f64(&self, key: &str, dflt: f64) -> f64 {
        self.raw(key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(dflt)
            .clamp(0.0, 1.0)
    }
}

impl VoiceKnobs {
    /// The dials an operator can set separately for a telephone line.
    ///
    /// Resolution itself is uniform: every dial is looked up under the line's prefix
    /// before the shared one, so this list decides only what the panel offers. It exists
    /// so the panel and the resolver cannot drift into a setting that can be changed and
    /// has no effect. Offering all of them would double the panel for decisions nobody
    /// makes.
    pub const PHONE_DIALS: [&'static str; 6] = [
        "silence_threshold_ms",
        "min_speech_ms",
        "turn_detection",
        "speech_rms",
        "barge_rms",
        "barge_min_ms",
    ];

    /// The dials a telephone line wants, before any operator override.
    ///
    /// Every difference from [`VoiceKnobs::default`] is a difference the transport
    /// forces, and each one is argued for where it is set. A dial not named here is
    /// deliberately the same on both.
    pub fn phone() -> Self {
        Self {
            // A tab shows that something is happening: a state pill, a waveform, a button
            // that is plainly held. A telephone shows nothing, so a pause with no reply
            // reads as a dropped call and the caller starts saying "hello?". Shorter is
            // only safe because the detector below holds a genuine mid-thought pause, and
            // because a detector that stops answering now falls back rather than waiting
            // for ever.
            silence_threshold_ms: 900,
            // A line carries background noise continuously, in a way a microphone in a
            // quiet room does not, and short bursts of it clear the loudness gate with
            // nobody speaking. Below this an utterance is discarded without being sent
            // for recognition, so the cost of getting this wrong is a transcript of
            // nothing. Still short enough to admit a clipped "yes".
            min_speech_ms: 250,
            // On by default here, unlike a tab. Without it the only thing that can end a
            // caller's turn is the silence timer, and with no screen the caller has no way
            // to tell a pause being waited out from a line that has gone dead. If the
            // sidecar is not configured this has no effect, and if it stops answering the
            // call falls back to the timer.
            turn_detection: true,
            // Interrupting on a line costs this plus the pacer's own prebuffer plus
            // whatever the carrier is holding, and 320 ms of talk-over before we even
            // signal is already at the edge of "it kept talking over me". What 320 ms
            // guards against does not arise here either: the echo canceller is the
            // carrier's, upstream, and a handset keeps the earpiece away from the
            // microphone. Twelve consecutive loud frames is still far more than a spike.
            barge_min_ms: 240,
            ..Self::default()
        }
    }

    /// Load the dials from the runtime config; an unset key keeps its default.
    pub async fn load(pg: &sqlx::PgPool) -> Self {
        Self::load_for(pg, VoiceProfile::Browser).await
    }

    /// Load the dials as `profile` wants them: its own rows first, then the shared ones,
    /// then that profile's compiled defaults.
    pub async fn load_for(pg: &sqlx::PgPool, profile: VoiceProfile) -> Self {
        Self::from_rows(&DialRows::load(pg, profile).await)
    }

    fn from_rows(rows: &DialRows) -> Self {
        let d = match rows.profile {
            VoiceProfile::Browser => Self::default(),
            VoiceProfile::Phone => Self::phone(),
        };
        VoiceKnobs {
            silence_threshold_ms: rows.get_u64("silence_threshold_ms", d.silence_threshold_ms),
            min_speech_ms: rows.get_u64("min_speech_ms", d.min_speech_ms),
            ptt_default: rows.get_bool("ptt_default", d.ptt_default),
            aec_required: rows.get_bool("aec_required", d.aec_required),
            turn_detection: rows.get_bool("turn_detection", d.turn_detection),
            speech_rms: rows.get_f64("speech_rms", d.speech_rms),
            barge_rms: rows.get_f64("barge_rms", d.barge_rms),
            barge_min_ms: rows.get_u64("barge_min_ms", d.barge_min_ms),
            spec_enabled: rows.get_bool("spec_enabled", d.spec_enabled),
            spec_min_words: rows.get_u64("spec_min_words", d.spec_min_words),
            spec_min_new_words: rows.get_u64("spec_min_new_words", d.spec_min_new_words),
            spec_debounce_ms: rows.get_u64("spec_debounce_ms", d.spec_debounce_ms),
            spec_max_fires: rows.get_u64("spec_max_fires", d.spec_max_fires),
            // Clamped to the same 10-90 the knob store accepts on write, so a value
            // that arrived by some other route behaves exactly like one typed in.
            spec_soft_silence_pct: rows
                .get_u64("spec_soft_silence_pct", d.spec_soft_silence_pct)
                .clamp(10, 90),
            spec_eager_prob: rows.get_f32("spec_eager_prob", d.spec_eager_prob),
            spec_timeout_secs: rows.get_u64("spec_timeout_secs", d.spec_timeout_secs),
            spec_reuse_jaccard: rows.get_f32("spec_reuse_jaccard", d.spec_reuse_jaccard),
            spec_reuse_new_ratio: rows.get_f32("spec_reuse_new_ratio", d.spec_reuse_new_ratio),
        }
    }

    /// The speculator's firing policy, as the decision core wants it.
    pub fn spec_cfg(&self) -> spec_retrieval::SpecCfg {
        spec_retrieval::SpecCfg {
            enabled: self.spec_enabled,
            min_words: self.spec_min_words as usize,
            min_new_words: self.spec_min_new_words as usize,
            debounce_ms: self.spec_debounce_ms,
            max_fires: self.spec_max_fires as u32,
            eager_prob: self.spec_eager_prob,
            soft_silence_pct: self.spec_soft_silence_pct,
        }
    }

    /// The reuse-gate thresholds.
    pub fn reuse_cfg(&self) -> spec_retrieval::ReuseCfg {
        spec_retrieval::ReuseCfg {
            jaccard: self.spec_reuse_jaccard,
            new_ratio: self.spec_reuse_new_ratio,
        }
    }
}

#[cfg(test)]
mod dial_tests {
    use super::*;

    fn rows(pairs: &[(&str, &str)]) -> DialRows {
        for_profile(VoiceProfile::Browser, pairs)
    }

    fn for_profile(profile: VoiceProfile, pairs: &[(&str, &str)]) -> DialRows {
        DialRows {
            rows: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            profile,
        }
    }

    /// Nothing set is every default. The store being unreachable resolves the same way,
    /// which is why an empty set has to mean this rather than mean zero.
    #[test]
    fn nothing_set_is_every_default() {
        let resolved = VoiceKnobs::from_rows(&rows(&[]));
        let d = VoiceKnobs::default();
        assert_eq!(resolved.silence_threshold_ms, d.silence_threshold_ms);
        assert_eq!(resolved.min_speech_ms, d.min_speech_ms);
        assert_eq!(resolved.ptt_default, d.ptt_default);
        assert_eq!(resolved.aec_required, d.aec_required);
        assert_eq!(resolved.turn_detection, d.turn_detection);
        assert_eq!(resolved.spec_enabled, d.spec_enabled);
        assert_eq!(resolved.spec_soft_silence_pct, d.spec_soft_silence_pct);
        assert_eq!(resolved.spec_eager_prob, d.spec_eager_prob);
    }

    #[test]
    fn a_set_dial_is_used() {
        let resolved = VoiceKnobs::from_rows(&rows(&[
            ("voice.silence_threshold_ms", "700"),
            ("voice.turn_detection", "true"),
            ("voice.spec_eager_prob", "0.9"),
        ]));
        assert_eq!(resolved.silence_threshold_ms, 700);
        assert!(resolved.turn_detection);
        assert_eq!(resolved.spec_eager_prob, 0.9);
    }

    /// A dial set to something unreadable keeps its default rather than becoming zero.
    /// Zero would open every gate it guards, which is the opposite of what a typo means.
    #[test]
    fn an_unreadable_dial_keeps_its_default() {
        let resolved = VoiceKnobs::from_rows(&rows(&[
            ("voice.silence_threshold_ms", "soon"),
            ("voice.spec_eager_prob", "quite likely"),
            ("voice.min_speech_ms", ""),
        ]));
        let d = VoiceKnobs::default();
        assert_eq!(resolved.silence_threshold_ms, d.silence_threshold_ms);
        assert_eq!(resolved.spec_eager_prob, d.spec_eager_prob);
        assert_eq!(resolved.min_speech_ms, d.min_speech_ms);
    }

    /// A switch that is set to anything other than on is off. Only an absent row means
    /// "whatever the default is".
    #[test]
    fn a_switch_set_to_anything_else_is_off() {
        let resolved = VoiceKnobs::from_rows(&rows(&[
            ("voice.spec_enabled", "no"),
            ("voice.ptt_default", ""),
        ]));
        assert!(!resolved.spec_enabled, "the default is on, so this proves the row was read");
        assert!(!resolved.ptt_default);
    }

    /// Fractions are clamped on the way in, and so is anything that arrived by some
    /// other route than the panel, because only whole numbers are range-checked on write.
    #[test]
    fn fractions_are_clamped_however_they_arrived() {
        let resolved = VoiceKnobs::from_rows(&rows(&[
            ("voice.spec_eager_prob", "9"),
            ("voice.spec_reuse_jaccard", "-2"),
            ("voice.spec_reuse_new_ratio", "inf"),
            ("voice.spec_soft_silence_pct", "99"),
        ]));
        assert_eq!(resolved.spec_eager_prob, 1.0);
        assert_eq!(resolved.spec_reuse_jaccard, 0.0);
        // Not finite, so it falls back, and the fallback is clamped on the same path.
        assert_eq!(resolved.spec_reuse_new_ratio, VoiceKnobs::default().spec_reuse_new_ratio);
        assert_eq!(resolved.spec_soft_silence_pct, 90);
    }

    /// A row for something else entirely must not be mistaken for a dial.
    #[test]
    fn a_neighbouring_key_is_not_a_dial() {
        let resolved = VoiceKnobs::from_rows(&rows(&[
            ("voice.tts_stream_url", "http://example.test"),
            ("voice.stt_sample_rate", "8000"),
        ]));
        let d = VoiceKnobs::default();
        assert_eq!(resolved.silence_threshold_ms, d.silence_threshold_ms);
    }

    /// The browser has no profile of its own, so the keys it reads are exactly the keys
    /// it has always read. This is the whole guarantee that a phone dial cannot move it.
    #[test]
    fn a_browser_has_no_profile_keys() {
        assert_eq!(VoiceProfile::Browser.override_key("silence_threshold_ms"), None);
        assert_eq!(
            VoiceProfile::Phone.override_key("silence_threshold_ms").as_deref(),
            Some("voice.phone.silence_threshold_ms")
        );
    }

    /// A dial set for the telephone must not reach the browser.
    #[test]
    fn a_phone_dial_does_not_move_the_browser() {
        let set = [("voice.phone.silence_threshold_ms", "300")];
        assert_eq!(
            VoiceKnobs::from_rows(&for_profile(VoiceProfile::Browser, &set)).silence_threshold_ms,
            VoiceKnobs::default().silence_threshold_ms
        );
        assert_eq!(
            VoiceKnobs::from_rows(&for_profile(VoiceProfile::Phone, &set)).silence_threshold_ms,
            300
        );
    }

    /// A change made for everybody reaches the line too. This is what two-level lookup
    /// means, and it is the thing most likely to surprise: an operator lowering the shared
    /// threshold for the browser will find the telephone follows.
    #[test]
    fn a_global_change_reaches_the_line_too() {
        let resolved = VoiceKnobs::from_rows(&for_profile(
            VoiceProfile::Phone,
            &[("voice.silence_threshold_ms", "700")],
        ));
        assert_eq!(resolved.silence_threshold_ms, 700);
    }

    /// And the line's own row wins over the shared one when both are set.
    #[test]
    fn the_lines_own_dial_wins_over_the_shared_one() {
        let resolved = VoiceKnobs::from_rows(&for_profile(
            VoiceProfile::Phone,
            &[("voice.silence_threshold_ms", "700"), ("voice.phone.silence_threshold_ms", "400")],
        ));
        assert_eq!(resolved.silence_threshold_ms, 400);
    }

    /// With nothing set at all, each transport gets its own compiled defaults.
    #[test]
    fn each_transport_falls_back_to_its_own_defaults() {
        let browser = VoiceKnobs::from_rows(&for_profile(VoiceProfile::Browser, &[]));
        let phone = VoiceKnobs::from_rows(&for_profile(VoiceProfile::Phone, &[]));
        assert_eq!(browser.barge_min_ms, VoiceKnobs::default().barge_min_ms);
        assert_eq!(phone.barge_min_ms, VoiceKnobs::phone().barge_min_ms);
    }

    /// The differences between the two profiles are deliberate, so they are pinned as
    /// relationships rather than as numbers: a later tidy-up that equalises them fails
    /// here instead of quietly making a telephone behave like a tab.
    #[test]
    fn the_phone_profile_differs_where_it_means_to() {
        let browser = VoiceKnobs::default();
        let phone = VoiceKnobs::phone();
        assert!(
            phone.barge_min_ms < browser.barge_min_ms,
            "a line has to be quicker to notice being talked over"
        );
        assert!(
            phone.silence_threshold_ms < browser.silence_threshold_ms,
            "a caller with no screen must not be left listening to nothing"
        );
        assert!(
            phone.min_speech_ms > browser.min_speech_ms,
            "a line's background noise must not be mistaken for speech"
        );
        assert!(
            phone.turn_detection && !browser.turn_detection,
            "a caller has nothing on screen to explain a pause, so the pause has to be judged"
        );
        // Placeholders awaiting measurement: equal on purpose, not by omission.
        assert_eq!(phone.speech_rms, browser.speech_rms);
        assert_eq!(phone.barge_rms, browser.barge_rms);
    }

    /// The shorter turn silence a line uses must still leave room for the speculative
    /// search's soft endpoint to be a distinct moment from the turn ending, or the head
    /// start it exists to buy is zero.
    #[test]
    fn a_lines_soft_endpoint_is_still_before_its_turn_ends() {
        let phone = VoiceKnobs::phone();
        let soft = phone.silence_threshold_ms * phone.spec_soft_silence_pct / 100;
        assert!(soft > 0 && soft < phone.silence_threshold_ms, "soft {soft}");
    }

    #[test]
    fn a_transport_labels_itself_with_a_fixed_name() {
        assert_eq!(VoiceProfile::Browser.as_str(), "browser");
        assert_eq!(VoiceProfile::Phone.as_str(), "phone");
    }
}

/// The live-voice **engine** config (STT/TTS endpoints + models + keys), resolved
/// fresh per session from the runtime config store with the boot `[voice_live]` as
/// fallback. Mirrors [`VoiceKnobs::load`]. API keys are
/// stored AES-256-GCM-encrypted under `voice.*_api_key_enc` (so the audit row only
/// holds ciphertext) and decrypted here with the deployment `message_key`.
#[derive(Debug, Clone)]
pub struct VoiceLiveResolved {
    pub stt_stream_kind: String, // none | websocket | openai_realtime
    pub stt_stream_url: String,
    pub stt_model: String,
    /// STT model for streaming **dictation** (composer mic). Distinct from the live-
    /// voice `stt_model`: dictation wants a live-delta transcription model
    /// (`gpt-realtime-whisper`) under server VAD. Falls back to `stt_model` if unset.
    pub dictation_model: String,
    pub stt_language: String,
    pub stt_sample_rate: u32,
    pub stt_api_key: Option<String>,
    pub tts_stream: bool,
    pub tts_stream_url: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_api_key: Option<String>,
    pub turn_detector_url: String,
}

impl VoiceLiveResolved {
    /// Config keys (all `voice.*`) so the admin endpoint, the generic Config editor
    /// filter, and this resolver agree on one list.
    pub const STR_KEYS: [&'static str; 10] = [
        "voice.stt_stream_kind",
        "voice.stt_stream_url",
        "voice.stt_model",
        "voice.dictation_model",
        "voice.stt_language",
        "voice.tts_stream_url",
        "voice.tts_model",
        "voice.tts_voice",
        "voice.turn_detector_url",
        "voice.stt_sample_rate", // int-as-string
    ];
    pub const ENC_KEYS: [&'static str; 2] = ["voice.stt_api_key_enc", "voice.tts_api_key_enc"];

    pub async fn load(pg: &sqlx::PgPool, message_key: Option<[u8; 32]>, boot: &crate::config::VoiceLiveConfig) -> Self {
        use crate::config::runtime;
        async fn gets(pg: &sqlx::PgPool, key: &str, dflt: &str) -> String {
            runtime::get(pg, key).await.ok().flatten().map(|e| e.value).filter(|v| !v.is_empty()).unwrap_or_else(|| dflt.to_string())
        }
        async fn getb(pg: &sqlx::PgPool, key: &str, dflt: bool) -> bool {
            runtime::get(pg, key).await.ok().flatten().map(|e| e.value == "true").unwrap_or(dflt)
        }
        // Decrypt a stored ciphertext key (None when unset or undecryptable).
        async fn getkey(pg: &sqlx::PgPool, key: &str, mk: Option<[u8; 32]>) -> Option<String> {
            let ct = runtime::get(pg, key).await.ok().flatten().map(|e| e.value).filter(|v| !v.is_empty())?;
            let _mk = mk?;
            match crate::crypto::decrypt_at_rest(&ct) {
                Ok(pt) => Some(pt),
                Err(_) => {
                    tracing::warn!(%key, "voice api key failed to decrypt; ignoring");
                    None
                }
            }
        }
        let sr = runtime::get(pg, "voice.stt_sample_rate").await.ok().flatten()
            .and_then(|e| e.value.parse::<u32>().ok()).unwrap_or(boot.stt_sample_rate);
        VoiceLiveResolved {
            stt_stream_kind: gets(pg, "voice.stt_stream_kind", &boot.stt_stream_kind).await,
            stt_stream_url: gets(pg, "voice.stt_stream_url", &boot.stt_stream_url).await,
            stt_model: gets(pg, "voice.stt_model", "").await,
            dictation_model: gets(pg, "voice.dictation_model", "gpt-realtime-whisper").await,
            stt_language: gets(pg, "voice.stt_language", "en").await,
            stt_sample_rate: sr.max(8_000),
            stt_api_key: getkey(pg, "voice.stt_api_key_enc", message_key).await,
            tts_stream: getb(pg, "voice.tts_stream", boot.tts_stream).await,
            tts_stream_url: gets(pg, "voice.tts_stream_url", &boot.tts_stream_url).await,
            tts_model: gets(pg, "voice.tts_model", "kokoro").await,
            tts_voice: gets(pg, "voice.tts_voice", "").await,
            tts_api_key: getkey(pg, "voice.tts_api_key_enc", message_key).await,
            turn_detector_url: gets(pg, "voice.turn_detector_url", &boot.turn_detector_url).await,
        }
    }

    /// Is a streaming-STT engine configured? Drives streaming dictation (else the
    /// composer mic falls back to batch transcription).
    pub fn has_streaming_stt(&self) -> bool {
        (self.stt_stream_kind == "websocket" && !self.stt_stream_url.is_empty())
            || (self.stt_stream_kind == "openai_realtime"
                && self.stt_api_key.as_deref().is_some_and(|k| !k.is_empty()))
    }
}
