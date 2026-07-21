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

//! Speculative retrieval for live voice — start the knowledge-base search from a
//! *partial* transcript, while the speaker is still talking, so the result is
//! already in hand when their turn commits.
//!
//! Without this, retrieval sits entirely after the speaker stops: it is charged in
//! full to the voice-to-voice budget, and the iterative pipeline takes seconds. The
//! speaker's pauses are dead time we can spend instead.
//!
//! The risk is obvious — a partial transcript may not be what the speaker meant. So
//! the design is speculate-then-check: shots fire on *stable* text only (a word
//! prefix two consecutive recognition hypotheses agree on, so a flickering
//! hypothesis cannot trigger one), and at commit a reuse gate compares the shot's
//! query with the final transcript. A shot that does not match is dropped and the
//! turn retrieves exactly as it would have anyway — the discard path is
//! indistinguishable from the feature being absent.
//!
//! This module is the pure core: no clock, no I/O, no database. Elapsed time and
//! endpoint signals arrive as plain arguments, exactly as in [`super::turn`], so
//! every decision is unit-testable in isolation. The orchestration types below the
//! divider carry the in-flight handle and the result pool; the session owns them.

// --- pure core ---------------------------------------------------------------

/// How a streaming-STT engine words its partials — they are not the same shape.
///
/// The websocket sidecar re-estimates and re-sends the **whole** hypothesis each
/// time, so consecutive partials can be compared for agreement. The OpenAI realtime
/// adapter emits **incremental** fragments instead, where each partial is new text
/// that will not be revised, so agreement is meaningless and fragments accumulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialMode {
    /// Each partial is a fresh estimate of the whole utterance so far.
    Cumulative,
    /// Each partial is a new fragment appended to what came before.
    Delta,
}

impl PartialMode {
    /// Pick the mode from the configured streaming-STT engine kind. Anything we do
    /// not recognise is treated as cumulative: the agreement step then simply holds
    /// fire until two hypotheses line up, which is the safe direction to be wrong in.
    pub fn for_engine(stt_stream_kind: &str) -> Self {
        match stt_stream_kind {
            "openai_realtime" => PartialMode::Delta,
            _ => PartialMode::Cumulative,
        }
    }
}

/// Firing policy for the speculator, lifted from the voice dials (already clamped
/// there, so this type carries no validation of its own).
#[derive(Debug, Clone)]
pub struct SpecCfg {
    /// Master switch.
    pub enabled: bool,
    /// Minimum words in the query before a shot is worth making.
    pub min_words: usize,
    /// Minimum growth since the previous shot, so we do not re-fire on "so um".
    pub min_new_words: usize,
    /// Minimum gap between shots.
    pub debounce_ms: u64,
    /// Cap on shots per utterance, eager included.
    pub max_fires: u32,
    /// Semantic turn-completeness probability that counts as a soft endpoint. Only
    /// meaningful when the turn-detection sidecar is configured; `1.0` disables it.
    pub eager_prob: f32,
    /// Soft endpoint as a percentage of the hard turn-ending silence threshold.
    pub soft_silence_pct: u64,
}

/// Reuse-gate thresholds.
#[derive(Debug, Clone, Copy)]
pub struct ReuseCfg {
    /// Token-Jaccard similarity at or above which a shot is reusable.
    pub jaccard: f32,
    /// When the shot's query is a word-prefix of the final transcript, the largest
    /// fraction of the final that may be words the shot never saw.
    pub new_ratio: f32,
}

/// Per-utterance speculator state. Owned as a local by the one task that reads STT
/// events, so it is never shared and never locked.
#[derive(Debug, Default, Clone)]
pub struct SpecState {
    /// The previous hypothesis, kept only to compute agreement with the latest.
    prev: Vec<String>,
    /// The newest hypothesis — the query for an eager shot, where near-final text
    /// beats a conservative prefix.
    latest: Vec<String>,
    /// The longest word prefix the last two hypotheses agree on.
    stable: Vec<String>,
    fired: u32,
    last_fire_ms: Option<u64>,
    last_query: String,
    /// The eager shot is once-per-utterance; this latches it.
    soft_fired: bool,
}

/// What the orchestrator should do about the transcript as it now stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecFire {
    /// Nothing to do.
    None,
    /// Fire on the stable prefix.
    Speculative(String),
    /// Fire on the full current hypothesis: the speaker is at a soft endpoint, so
    /// this is very likely the text the turn will actually commit.
    Eager(String),
}

/// Split a string into comparable words: lowercased, stripped of punctuation.
/// Everything the gate and the decider compare goes through here, so "Holiday?" and
/// "holiday" are the same word.
pub fn normalise(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Length of the longest common word prefix of two hypotheses. Agreement between
/// consecutive hypotheses is what makes a prefix "stable": text both estimates share
/// is text the recogniser has stopped revising.
pub fn agree_len(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

impl SpecState {
    /// Fold one partial in.
    pub fn observe(&mut self, text: &str, mode: PartialMode) {
        let words = normalise(text);
        if words.is_empty() {
            return;
        }
        match mode {
            PartialMode::Cumulative => {
                self.prev = std::mem::take(&mut self.latest);
                self.latest = words;
                let n = agree_len(&self.prev, &self.latest);
                self.stable = self.latest[..n].to_vec();
            }
            PartialMode::Delta => {
                // A delta is text the engine has already settled on, so there is
                // nothing to agree about: append, and treat all of it as stable.
                self.latest.extend(words);
                self.stable = self.latest.clone();
            }
        }
    }

    /// The stable prefix as a query string (test and diagnostics helper).
    pub fn stable_query(&self) -> String {
        self.stable.join(" ")
    }

    /// Record that a shot went out, so debounce, growth and the cap all advance.
    pub fn note_fired(&mut self, query: &str, now_ms: u64, eager: bool) {
        self.fired += 1;
        self.last_fire_ms = Some(now_ms);
        self.last_query = query.to_string();
        if eager {
            self.soft_fired = true;
        }
    }

    /// Clear for the next utterance: on turn commit, on barge-in, on teardown.
    pub fn reset(&mut self) {
        *self = SpecState::default();
    }

    /// Shots made so far this utterance.
    pub fn fired(&self) -> u32 {
        self.fired
    }
}

/// Decide whether to fire a speculative retrieval right now.
///
/// `soft_endpoint` is the early half of two-threshold endpointing: the speaker has
/// paused long enough (or the semantic detector is confident enough) that the turn
/// is probably ending, but not long enough to commit it. That is the moment the
/// hypothesis is most nearly final and a shot is most likely to survive the gate, so
/// the growth and debounce rules are relaxed for it — the cap is not.
pub fn decide(
    st: &SpecState,
    cfg: &SpecCfg,
    now_ms: u64,
    soft_endpoint: bool,
    kb_present: bool,
) -> SpecFire {
    if !cfg.enabled || !kb_present || st.fired >= cfg.max_fires {
        return SpecFire::None;
    }

    if soft_endpoint && !st.soft_fired {
        let q = st.latest.join(" ");
        // Even at the endpoint, a two-word utterance is not worth a retrieval, and
        // re-firing the query we already have in flight buys nothing.
        if st.latest.len() >= cfg.min_words && normalise(&q) != normalise(&st.last_query) {
            return SpecFire::Eager(q);
        }
        return SpecFire::None;
    }

    if st.stable.len() < cfg.min_words {
        return SpecFire::None;
    }
    let prev_len = normalise(&st.last_query).len();
    if st.stable.len().saturating_sub(prev_len) < cfg.min_new_words {
        return SpecFire::None;
    }
    if let Some(last) = st.last_fire_ms {
        if now_ms.saturating_sub(last) < cfg.debounce_ms {
            return SpecFire::None;
        }
    }
    SpecFire::Speculative(st.stable.join(" "))
}

/// Whether a completed (or in-flight) shot's query is close enough to the committed
/// transcript to answer it.
///
/// Two ways to pass. The speaker finished the sentence we speculated on — the shot's
/// query is a word-prefix of the final and the tail it never saw is a small fraction
/// of the whole. Or they re-worded rather than extended, and the two still share most
/// of their words. Anything else is a different question, and reusing it would answer
/// something the speaker did not ask.
pub fn reuse_ok(pool_query: &str, final_text: &str, cfg: ReuseCfg) -> bool {
    let p = normalise(pool_query);
    let f = normalise(final_text);
    if p.is_empty() || f.is_empty() {
        return false;
    }

    if p.len() <= f.len() && f[..p.len()] == p[..] {
        let new = (f.len() - p.len()) as f32 / f.len() as f32;
        if new <= cfg.new_ratio {
            return true;
        }
    }

    let ps: std::collections::HashSet<&String> = p.iter().collect();
    let fs: std::collections::HashSet<&String> = f.iter().collect();
    let inter = ps.intersection(&fs).count() as f32;
    let union = ps.union(&fs).count() as f32;
    union > 0.0 && inter / union >= cfg.jaccard
}

/// Turn a resolved retrieval allow-list and deny-list into the arguments for a shot,
/// or refuse to shoot.
///
/// Fail-closed, and deliberately so: a shot reads the speaker's knowledge bases with
/// their permissions, so anything short of a clean resolve — an error either side, or
/// an empty allow-list — means no speculation at all rather than a broader search.
#[allow(clippy::result_unit_err)]
pub fn acl_or_none(
    allow: Result<Vec<uuid::Uuid>, ()>,
    deny: Result<Vec<uuid::Uuid>, ()>,
) -> Option<(Vec<String>, Vec<String>)> {
    let allow = allow.ok()?;
    let deny = deny.ok()?;
    if allow.is_empty() {
        return None;
    }
    Some((
        allow.iter().map(|id| id.to_string()).collect(),
        deny.iter().map(|id| id.to_string()).collect(),
    ))
}

// --- orchestration -----------------------------------------------------------

/// A completed speculative retrieval, waiting for a turn to claim it.
#[derive(Debug, Clone)]
pub struct SpecResult {
    /// The transcript the shot was fired on — what the reuse gate judges.
    pub query: String,
    pub context: String,
    pub citations: Vec<crate::ml::Citation>,
    pub parts: Vec<crate::ml::SynthPart>,
    pub debug: crate::ml::RetrieveDebug,
    /// How long the shot itself took: the time a reusing turn does not spend.
    pub shot_ms: u64,
}

/// A shot that has not finished yet.
///
/// Aborting the handle drops the retrieval stream inside the task, and that stream's
/// `Drop` cancels the upstream HTTP body — so cancellation costs nothing and needs no
/// cooperation from the ML service.
#[derive(Debug)]
pub struct Shot {
    pub seq: u64,
    pub query: String,
    pub started: std::time::Instant,
    /// Yields the result when the search finished too late to be parked in the pool
    /// — see [`SpecShared::epoch`]. `None` means it was parked, or produced nothing.
    pub handle: tokio::task::JoinHandle<Option<SpecResult>>,
}

/// Counters for one utterance. Defaults are calibrated from these, so they are not
/// optional.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpecStats {
    pub fires: u32,
    pub fires_eager: u32,
    /// Shots dropped in flight, by a newer shot or by barge-in.
    pub cancelled: u32,
}

/// What the turn did with the speculation, for the turn log and the diagnostics
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecOutcome {
    /// A finished shot passed the gate.
    Reused,
    /// An unfinished shot passed the gate and the turn waited for it.
    ReusedAwaited,
    /// A shot existed but did not match the committed transcript.
    DiscardedGate,
    /// Nothing to reuse: no shot was made, or none had returned.
    DiscardedNone,
    /// The speculator is switched off.
    Disabled,
}

impl SpecOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecOutcome::Reused => "reused",
            SpecOutcome::ReusedAwaited => "reused_awaited",
            SpecOutcome::DiscardedGate => "discarded_gate",
            SpecOutcome::DiscardedNone => "discarded_none",
            SpecOutcome::Disabled => "disabled",
        }
    }
}

/// The speculator state shared between the capture loop (which fires) and the turn
/// (which claims). Held behind the session's mutex; every field is cleared when the
/// utterance ends, so nothing survives into the next turn.
#[derive(Debug, Default)]
pub struct SpecShared {
    pub next_seq: u64,
    pub inflight: Option<Shot>,
    /// The most recent completed shot — one only; a newer result overwrites.
    pub pool: Option<SpecResult>,
    /// Allow-list and deny-list resolved for this utterance. Scoped to the
    /// utterance so repeated shots cannot disagree with each other, and so nothing
    /// is ever carried across a turn: the committed turn always re-resolves before
    /// any of it is used.
    pub acl: Option<(Vec<String>, Vec<String>)>,
    pub stats: SpecStats,
    /// Which utterance the pool belongs to.
    ///
    /// A search runs on its own task, so it can finish at any moment — including
    /// just as the turn it was speculating for is being committed or abandoned.
    /// Without a fence, such a late result would be parked in the pool and picked up
    /// by the *next* turn, answering a question nobody asked.
    ///
    /// So a search records the epoch it started under and may only park its result
    /// while that epoch still stands. [`SpecShared::close`] bumps the epoch and
    /// empties the pool in one critical section, which settles the race whichever
    /// way it falls: a result parked before the bump is cleared, and one arriving
    /// after is refused. It is then delivered up the task's join handle instead, so
    /// a turn deliberately waiting for that search still gets it.
    pub epoch: u64,
}

impl SpecShared {
    /// End the current utterance's speculation: nothing may be parked from now on,
    /// and anything already parked is discarded.
    pub fn close(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pool = None;
    }

    /// Whether a search that started under `epoch` may still park its result.
    pub fn admits(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// Drop everything speculative and abort any search still running. Called on
    /// barge-in and on teardown.
    pub fn clear(&mut self) {
        if let Some(s) = self.inflight.take() {
            s.handle.abort();
            self.stats.cancelled += 1;
        }
        self.close();
        self.acl = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SpecCfg {
        SpecCfg {
            enabled: true,
            min_words: 5,
            min_new_words: 4,
            debounce_ms: 700,
            max_fires: 3,
            eager_prob: 0.4,
            soft_silence_pct: 50,
        }
    }

    fn reuse_cfg() -> ReuseCfg {
        ReuseCfg { jaccard: 0.7, new_ratio: 0.35 }
    }

    /// Drive a sequence of cumulative hypotheses in, firing whenever the decider
    /// says so, and report the queries that went out.
    fn run(hyps: &[(&str, u64)], c: &SpecCfg) -> Vec<String> {
        let mut st = SpecState::default();
        let mut out = Vec::new();
        for (text, now) in hyps {
            st.observe(text, PartialMode::Cumulative);
            if let SpecFire::Speculative(q) = decide(&st, c, *now, false, true) {
                st.note_fired(&q, *now, false);
                out.push(q);
            }
        }
        out
    }

    #[test]
    fn first_hypothesis_never_fires() {
        // Nothing to agree with yet, so the stable prefix is empty by construction —
        // one hypothesis is an opinion, not a settled transcript.
        let mut st = SpecState::default();
        st.observe("what is the holiday allowance for contractors", PartialMode::Cumulative);
        assert_eq!(st.stable_query(), "");
        assert_eq!(decide(&st, &cfg(), 0, false, true), SpecFire::None);
    }

    #[test]
    fn fires_once_two_hypotheses_agree() {
        let fired = run(
            &[
                ("what is the holiday", 0),
                ("what is the holiday allowance for contractors", 100),
            ],
            &cfg(),
        );
        // The agreed prefix is the four words both estimates share — below the
        // five-word floor, so still no shot.
        assert!(fired.is_empty(), "four agreed words is under min_words");

        let fired = run(
            &[
                ("what is the holiday allowance for", 0),
                ("what is the holiday allowance for contractors", 100),
            ],
            &cfg(),
        );
        assert_eq!(fired, vec!["what is the holiday allowance for".to_string()]);
    }

    #[test]
    fn oscillating_hypotheses_do_not_fire() {
        // The recogniser keeps changing its mind about the same audio. Agreement
        // collapses to nothing each time, so no shot is made on text that is about
        // to be revised away.
        let fired = run(
            &[
                ("book a flight to glasgow next", 0),
                ("look at fright to glasgow next", 800),
                ("book a flight to glasgow next", 1600),
                ("look at fright to glasgow next", 2400),
            ],
            &cfg(),
        );
        assert!(fired.is_empty(), "a flickering hypothesis must not trigger retrieval");
    }

    #[test]
    fn growth_and_debounce_both_gate_the_second_shot() {
        let c = cfg();
        let mut st = SpecState::default();
        st.observe("what is the holiday allowance for", PartialMode::Cumulative);
        st.observe("what is the holiday allowance for", PartialMode::Cumulative);
        let q = match decide(&st, &c, 0, false, true) {
            SpecFire::Speculative(q) => q,
            other => panic!("expected a shot, got {other:?}"),
        };
        st.note_fired(&q, 0, false);

        // Three new words is under min_new_words: not worth a second shot.
        st.observe("what is the holiday allowance for part time staff", PartialMode::Cumulative);
        st.observe("what is the holiday allowance for part time staff", PartialMode::Cumulative);
        assert_eq!(decide(&st, &c, 5_000, false, true), SpecFire::None, "growth gate");

        // Four new words clears growth — but not the debounce window.
        st.observe("what is the holiday allowance for part time staff members", PartialMode::Cumulative);
        st.observe("what is the holiday allowance for part time staff members", PartialMode::Cumulative);
        assert_eq!(decide(&st, &c, 500, false, true), SpecFire::None, "debounce gate");
        assert!(matches!(decide(&st, &c, 800, false, true), SpecFire::Speculative(_)));
    }

    #[test]
    fn cap_stops_firing() {
        let mut c = cfg();
        c.max_fires = 2;
        c.min_new_words = 1;
        c.debounce_ms = 0;
        let mut st = SpecState::default();
        let mut fired = 0;
        for (i, text) in [
            "what is the holiday allowance",
            "what is the holiday allowance for",
            "what is the holiday allowance for part",
            "what is the holiday allowance for part time",
            "what is the holiday allowance for part time staff",
        ]
        .iter()
        .enumerate()
        {
            // Each hypothesis is confirmed by a repeat so the prefix is stable.
            st.observe(text, PartialMode::Cumulative);
            st.observe(text, PartialMode::Cumulative);
            if let SpecFire::Speculative(q) = decide(&st, &c, i as u64 * 1000, false, true) {
                st.note_fired(&q, i as u64 * 1000, false);
                fired += 1;
            }
        }
        assert_eq!(fired, 2, "the cap is absolute");
    }

    #[test]
    fn eager_fires_on_the_full_hypothesis_once() {
        let c = cfg();
        let mut st = SpecState::default();
        st.observe("what is the holiday allowance for contractors", PartialMode::Cumulative);
        // No agreement yet, so the ordinary rule holds fire...
        assert_eq!(decide(&st, &c, 0, false, true), SpecFire::None);
        // ...but at a soft endpoint the newest hypothesis is nearly final, and that
        // is the shot worth making.
        let q = match decide(&st, &c, 0, true, true) {
            SpecFire::Eager(q) => q,
            other => panic!("expected an eager shot, got {other:?}"),
        };
        assert_eq!(q, "what is the holiday allowance for contractors");
        st.note_fired(&q, 0, true);
        // Latched: the soft endpoint does not keep re-firing while silence runs on.
        assert_eq!(decide(&st, &c, 50, true, true), SpecFire::None);
    }

    #[test]
    fn eager_respects_the_word_floor_and_the_cap() {
        let mut c = cfg();
        let mut st = SpecState::default();
        st.observe("no idea", PartialMode::Cumulative);
        assert_eq!(decide(&st, &c, 0, true, true), SpecFire::None, "too short to be worth a shot");

        c.max_fires = 0;
        st.observe("what is the holiday allowance for contractors", PartialMode::Cumulative);
        assert_eq!(decide(&st, &c, 0, true, true), SpecFire::None, "the cap covers eager too");
    }

    #[test]
    fn disabled_and_kbless_sessions_never_fire() {
        let mut c = cfg();
        let mut st = SpecState::default();
        st.observe("what is the holiday allowance for contractors", PartialMode::Cumulative);
        st.observe("what is the holiday allowance for contractors", PartialMode::Cumulative);
        assert_eq!(decide(&st, &c, 0, false, false), SpecFire::None, "no knowledge base bound");
        c.enabled = false;
        assert_eq!(decide(&st, &c, 0, false, true), SpecFire::None, "switched off");
    }

    #[test]
    fn reset_clears_the_utterance() {
        let c = cfg();
        let mut st = SpecState::default();
        st.observe("what is the holiday allowance for", PartialMode::Cumulative);
        st.observe("what is the holiday allowance for", PartialMode::Cumulative);
        let q = match decide(&st, &c, 0, false, true) {
            SpecFire::Speculative(q) => q,
            other => panic!("expected a shot, got {other:?}"),
        };
        st.note_fired(&q, 0, false);
        assert_eq!(st.fired(), 1);

        // Barge-in, commit, teardown: the next utterance starts from nothing, so a
        // stale prefix cannot leak into it.
        st.reset();
        assert_eq!(st.fired(), 0);
        assert_eq!(st.stable_query(), "");
        assert_eq!(decide(&st, &c, 0, false, true), SpecFire::None);
    }

    #[test]
    fn delta_partials_accumulate_instead_of_agreeing() {
        // Fragments from an incremental engine are already settled text; comparing
        // consecutive fragments for a common prefix would agree on nothing and the
        // speculator would never fire at all.
        let c = cfg();
        let mut st = SpecState::default();
        for frag in ["what is", "the holiday", "allowance"] {
            st.observe(frag, PartialMode::Delta);
        }
        assert_eq!(st.stable_query(), "what is the holiday allowance");
        assert!(matches!(decide(&st, &c, 0, false, true), SpecFire::Speculative(_)));
    }

    #[test]
    fn engine_kind_picks_the_partial_mode() {
        assert_eq!(PartialMode::for_engine("openai_realtime"), PartialMode::Delta);
        assert_eq!(PartialMode::for_engine("websocket"), PartialMode::Cumulative);
        // An unknown engine holds fire rather than guessing — the safe direction.
        assert_eq!(PartialMode::for_engine(""), PartialMode::Cumulative);
    }

    // --- reuse gate ---

    #[test]
    fn gate_accepts_the_sentence_being_finished() {
        let c = reuse_cfg();
        assert!(reuse_ok(
            "what is the holiday allowance for part time",
            "what is the holiday allowance for part time staff",
            c
        ));
        // The same text, however it was punctuated and capitalised on the way.
        assert!(reuse_ok(
            "What is the holiday allowance for part-time staff?",
            "what is the holiday allowance for part time staff",
            c
        ));
    }

    #[test]
    fn gate_rejects_a_long_tail_after_the_prefix() {
        let c = reuse_cfg();
        // The speculated words are a prefix, but most of the question came after
        // them — the shot never saw what was actually being asked.
        assert!(!reuse_ok(
            "and what about",
            "and what about the notice period for contractors on fixed term deals",
            c
        ));
    }

    #[test]
    fn gate_accepts_a_rewording() {
        let c = reuse_cfg();
        // Not a prefix at all, but the same question re-ordered.
        assert!(reuse_ok(
            "the holiday allowance for part time staff",
            "holiday allowance for the part time staff",
            c
        ));
    }

    #[test]
    fn gate_rejects_a_change_of_subject() {
        let c = reuse_cfg();
        assert!(!reuse_ok(
            "tell me about the holiday policy",
            "actually no what about the redundancy terms",
            c
        ));
    }

    #[test]
    fn gate_edges_are_closed() {
        let c = reuse_cfg();
        assert!(!reuse_ok("", "what is the holiday allowance", c), "no query, no reuse");
        assert!(!reuse_ok("what is the holiday allowance", "", c), "no transcript, no reuse");
        assert!(!reuse_ok("   ?!  ", "  ...  ", c), "punctuation alone is not a query");
        // A query longer than the final cannot be a prefix of it; it falls through
        // to the similarity arm rather than indexing past the end.
        assert!(reuse_ok("what is the holiday allowance for staff", "what is the holiday allowance for", c));
        assert!(reuse_ok("holiday allowance", "holiday allowance", c), "identical always reuses");
    }

    // --- ACL ---

    #[test]
    fn acl_is_fail_closed() {
        let kb = uuid::Uuid::now_v7();
        let doc = uuid::Uuid::now_v7();

        let ok = acl_or_none(Ok(vec![kb]), Ok(vec![doc])).expect("a clean resolve shoots");
        assert_eq!(ok.0, vec![kb.to_string()]);
        assert_eq!(ok.1, vec![doc.to_string()]);

        assert!(acl_or_none(Err(()), Ok(vec![])).is_none(), "allow-list error → no shot");
        assert!(acl_or_none(Ok(vec![kb]), Err(())).is_none(), "deny-list error → no shot");
        assert!(acl_or_none(Ok(vec![]), Ok(vec![])).is_none(), "nothing to search → no shot");
    }
}
