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

//! The transport for a live-voice session carried on a telephone line.
//!
//! Most of what a session reports has nowhere to go on a phone. There is no screen,
//! so the conversation state, the interim words and the settled transcript are
//! logged and no more. What is left is the reply audio, and converting it is the
//! whole job: raw samples from the synthesiser become narrowband frames released on
//! a clock.
//!
//! Nothing here knows which carrier is on the other end. The one carrier-specific
//! act, telling it to abandon what it has buffered, leaves as a [`Control`] message
//! for whoever is writing to the socket to render.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, Notify};

use super::codec::{self, Framer, Resampler, MULAW_FRAME_BYTES, MULAW_SILENCE};
use super::pace::{MarkId, MarkKind, PacerHandle};
use crate::voice::sink::{AudioClip, AudioDelivery, VoiceSink};
use crate::voice::VoiceState;

/// The rate the synthesisers produce raw samples at, and therefore the rate this
/// converts from.
const SYNTH_RATE: u32 = 24_000;

/// How long to wait for the far end to confirm it has finished playing a reply.
///
/// A bound, not a timeout to tune. Nothing obliges a carrier to echo a playback mark, and
/// one that does not would otherwise leave the session believing it is still speaking for
/// ever: from that moment every word the caller says is discarded rather than heard. So
/// the wait gives up, and gives up on the whole idea for the rest of the call, which is why
/// a carrier that cannot do this costs one second once rather than one second a sentence.
const MARK_GRACE: Duration = Duration::from_secs(1);

/// How a transport wants the reply's audio on the wire.
///
/// Two carriers, two answers, and the difference is a byte format rather than a design:
/// one carries companded narrowband, the other raw samples at the same rate. Everything
/// upstream of here is identical, which is the point of the transport being a seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// G.711 mu-law, 160 bytes to a twenty millisecond frame.
    Mulaw,
    /// Signed sixteen-bit little-endian samples, 320 bytes to the same frame.
    Pcm16,
}

impl Wire {
    /// How many bytes make one twenty millisecond frame.
    fn frame_bytes(self) -> usize {
        match self {
            Wire::Mulaw => MULAW_FRAME_BYTES,
            Wire::Pcm16 => MULAW_FRAME_BYTES * 2,
        }
    }

    /// What silence is, for padding the tail of a reply out to a whole frame.
    fn silence(self) -> u8 {
        match self {
            Wire::Mulaw => MULAW_SILENCE,
            // Two zero bytes are a zero sample, so one zero byte pads either half of one.
            Wire::Pcm16 => 0x00,
        }
    }

    /// Narrowband samples as this wire carries them.
    fn bytes(self, narrow: &[i16]) -> Vec<u8> {
        match self {
            Wire::Mulaw => codec::encode(narrow),
            Wire::Pcm16 => narrow.iter().flat_map(|s| s.to_le_bytes()).collect(),
        }
    }
}

/// Something the transport must do that is not audio.
pub enum Control {
    /// Tell the carrier to abandon the audio it has buffered but not yet played.
    Clear,
}

/// The reply-audio conversion: 24 kHz samples in, narrowband frames out.
///
/// Held together because the three parts are always used in one order and each
/// depends on where the last left off.
struct Downlink {
    res: Resampler,
    framer: Framer,
    /// The low byte of a sample split across two chunks.
    spare: Option<u8>,
    wire: Wire,
}

impl Downlink {
    fn new(wire: Wire) -> Self {
        Self {
            res: Resampler::down_24k_to_8k(),
            framer: Framer::new(wire.frame_bytes()),
            spare: None,
            wire,
        }
    }

    /// Convert one chunk of samples into whole frames, carrying the remainder.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        // A chunk from the engine is a network chunk, so it can end in the middle of
        // a sample. Rejoining the halves is not a nicety: read as whole samples from
        // the wrong byte, the rest of the call is loud static rather than a click.
        let mut src = Vec::with_capacity(bytes.len() + 1);
        if let Some(b) = self.spare.take() {
            src.push(b);
        }
        src.extend_from_slice(bytes);
        if src.len() % 2 == 1 {
            self.spare = src.pop();
        }
        if src.is_empty() {
            return Vec::new();
        }
        let samples: Vec<i16> =
            src.chunks_exact(2).map(|p| i16::from_le_bytes([p[0], p[1]])).collect();
        let narrow = self.res.process(&samples);
        self.framer.push(&self.wire.bytes(&narrow))
    }

    /// The tail of the reply, padded out to a whole frame.
    fn flush(&mut self) -> Option<Vec<u8>> {
        self.framer.flush(self.wire.silence())
    }

    /// Throw away the part-built frame and any split sample, and keep the filter
    /// history.
    ///
    /// The asymmetry is deliberate. Half a frame of an abandoned sentence carried
    /// into the next one is both an audible glitch and a lasting misalignment,
    /// whereas the filter history is what makes consecutive frames join smoothly:
    /// discarding it puts a click at the start of every reply.
    fn abandon(&mut self) {
        self.framer = Framer::new(self.wire.frame_bytes());
        self.spare = None;
    }
}

/// What the far end has confirmed playing.
///
/// The whole reason a mark exists: handing audio over and it being heard are different
/// moments, and only the far end knows the second one.
struct Marks {
    /// Which reply is being spoken. Bumped for each one, and again when one is abandoned,
    /// so a late confirmation of an abandoned reply is recognisably not about this one.
    generation: AtomicU64,
    /// The newest generation the far end has confirmed finishing.
    heard: AtomicU64,
    /// Woken when that changes, or when a reply is abandoned.
    changed: Notify,
    /// The far end does not echo marks. Latched on the first time it fails to, so the cost
    /// of a carrier that cannot do this is paid once rather than on every sentence.
    unsupported: AtomicBool,
}

/// A live-voice session's output, carried on a telephone line.
pub struct TelephonySink {
    pacer: PacerHandle,
    /// Out of band of the pacer, because a message telling the carrier to abandon
    /// its buffer must not queue behind the very audio it is abandoning.
    control: mpsc::Sender<Control>,
    down: Mutex<Downlink>,
    marks: Marks,
    /// The clock of the reply being spoken, so the moments only this end can observe are
    /// recorded against the same origin as every other stage of the turn.
    clock: Mutex<Option<std::sync::Arc<crate::voice::session::TurnClock>>>,
    /// This reply has had its opening mark placed.
    began: AtomicBool,
    /// How this call is to end, when something has asked for it to.
    ///
    /// Held here because this is the one place that knows when the caller has actually
    /// heard the reply. An agent putting somebody through decides during the turn, and
    /// acting on that decision the moment it is made would take the line away in the
    /// middle of the sentence explaining it.
    ending: crate::telephony::Ending,
}

impl TelephonySink {
    /// Build a sink over a transport.
    ///
    /// `confirms_playback` is whether the far end will ever say it has played what it was
    /// given. A carrier that buffers audio of its own does, and the confirmation is the
    /// only moment this end can know what the caller actually heard. A telephone system
    /// taking frames straight off a socket does not, and it has no need to: the pacer
    /// releases a frame every twenty milliseconds, so what has left here has been played.
    ///
    /// Told rather than discovered, and that is the whole reason it is an argument. Left to
    /// find out, the first reply would wait out the grace period for a confirmation that
    /// cannot come, and would then give up on playback reporting for the rest of the call,
    /// which is a second of silence in the wrong place and no timing figures afterwards.
    pub fn new(
        pacer: PacerHandle,
        control: mpsc::Sender<Control>,
        ending: crate::telephony::Ending,
        wire: Wire,
        confirms_playback: bool,
    ) -> Self {
        Self {
            pacer,
            control,
            down: Mutex::new(Downlink::new(wire)),
            marks: Marks {
                // Replies are numbered from one, so nought can mean "nothing confirmed
                // yet" without being a reply that has been.
                generation: AtomicU64::new(1),
                heard: AtomicU64::new(0),
                changed: Notify::new(),
                unsupported: AtomicBool::new(!confirms_playback),
            },
            clock: Mutex::new(None),
            began: AtomicBool::new(false),
            ending,
        }
    }

    /// The far end has played everything up to the point `name` stands for.
    ///
    /// Called from whatever is reading the carrier's messages, which is a different task
    /// from the one speaking, so this is where the two meet.
    pub async fn mark_echoed(&self, name: &str) {
        let Some(id) = MarkId::parse(name) else {
            // Not a mark this process wrote. Somebody else's software may be putting marks
            // on the same line, and reading one of those as ours would confirm a reply that
            // is still playing.
            return;
        };
        let current = self.marks.generation.load(Ordering::SeqCst);
        if id.generation != current {
            // An echo from a reply that was interrupted or has already finished.
            return;
        }
        let clock = { self.clock.lock().unwrap().clone() };
        if let Some(clock) = clock {
            match id.kind {
                MarkKind::FirstAudio => clock.reply_heard(),
                MarkKind::ReplyEnd => clock.reply_spoken(),
            }
        }
        if id.kind == MarkKind::ReplyEnd {
            self.marks.heard.store(id.generation, Ordering::SeqCst);
            self.marks.changed.notify_waiters();
        }
    }

    /// Wait for the far end to confirm it has finished playing this reply.
    ///
    /// Returns early, and for good, if it does not answer: see [`MARK_GRACE`].
    async fn await_reply_end(&self, generation: u64) {
        if self.marks.unsupported.load(Ordering::SeqCst) {
            return;
        }
        let confirmed = async {
            loop {
                // Registered before the value is read, so a confirmation arriving in
                // between wakes this rather than being missed.
                let changed = self.marks.changed.notified();
                if self.marks.heard.load(Ordering::SeqCst) >= generation
                    || self.marks.generation.load(Ordering::SeqCst) != generation
                {
                    return;
                }
                changed.await;
            }
        };
        if tokio::time::timeout(MARK_GRACE, confirmed).await.is_err() {
            if !self.marks.unsupported.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    "the carrier does not report when it has played our audio; the figures for what the caller heard will be absent"
                );
                metrics::counter!("telephony_mark_timeout_total").increment(1);
            }
        }
    }
}

#[async_trait]
impl VoiceSink for TelephonySink {
    async fn state(&self, state: VoiceState, retrieving: bool) {
        // Nothing to show a caller. Logged so a call can be followed as it happens.
        tracing::debug!(state = state.as_str(), retrieving, "call state");
    }

    async fn partial(&self, text: &str) {
        // The length only. Interim recognition of what a caller is part-way through
        // saying is the least reliable and most sensitive text in the system, and it
        // has no destination here.
        tracing::debug!(chars = text.chars().count(), "interim recognition");
    }

    async fn transcript(&self, text: &str) {
        // Not put on the wire: the turn persists it as the caller's message anyway.
        tracing::debug!(chars = text.chars().count(), "caller finished speaking");
    }

    async fn audio(&self, clip: AudioClip) {
        let frames = {
            let mut down = self.down.lock().unwrap();
            down.feed(&clip.bytes)
        };
        metrics::counter!("telephony_frames_out_total").increment(frames.len() as u64);
        let opening = !frames.is_empty() && !self.began.swap(true, Ordering::SeqCst);
        for frame in frames {
            self.pacer.push(frame).await;
        }
        if opening && !self.marks.unsupported.load(Ordering::SeqCst) {
            // Straight after the first audio of the reply, so its confirmation is the
            // moment the caller began hearing it. That is the figure the latency budget is
            // really about: everything before it is ours, and the gap between handing the
            // audio over and this is the line's own.
            let generation = self.marks.generation.load(Ordering::SeqCst);
            self.pacer.mark(MarkId { generation, kind: MarkKind::FirstAudio }).await;
        }
    }

    async fn audio_end(&self) {
        // Send the tail, but leave the pacer running: everything queued still has to
        // play out, and closing it here would cut the end off every reply.
        let tail = {
            let mut down = self.down.lock().unwrap();
            down.flush()
        };
        if let Some(frame) = tail {
            self.pacer.push(frame).await;
        }
        // And then wait for the line to actually play it.
        //
        // Synthesis finishes seconds before a telephone has said the words. Returning
        // here as soon as the last frame was handed over would have the session decide
        // it had stopped speaking while the caller is still listening: from that moment
        // the caller talking is read as a new question instead of an interruption, so
        // they get talked over by an answer to something they never asked, and the
        // reply they were interrupting plays on underneath it.
        //
        // Two waits, because they answer different questions. The first is when the last
        // frame left here; the second is when the far end says it played it, which is later
        // by however much it had buffered and is the only version of "finished speaking"
        // the caller would recognise.
        let generation = self.marks.generation.load(Ordering::SeqCst);
        self.pacer.mark(MarkId { generation, kind: MarkKind::ReplyEnd }).await;
        self.pacer.wait_drained().await;
        self.await_reply_end(generation).await;
        // This reply is over, whatever happened. The next one is a new generation, so
        // nothing that arrives late about this one can be mistaken for it.
        self.marks.generation.fetch_add(1, Ordering::SeqCst);
        self.began.store(false, Ordering::SeqCst);
        // And if something during this turn asked for the call to end, this is the
        // moment: the far end has confirmed it played the last of what was said, so the
        // caller has heard the sentence that goes with the decision. Doing it when the
        // decision was made would cut them off part-way through being told.
        if self.ending.end_as_asked() {
            tracing::info!("the reply has been heard; ending the call as asked");
        }
    }

    async fn error(&self, message: &str) {
        // No spoken apology: saying anything needs synthesis, which is what has just
        // failed. The caller hears silence until the turn ends and they can speak
        // again, which is a real gap in the experience rather than a hidden one.
        tracing::warn!(%message, "call error");
        metrics::counter!("telephony_error_total").increment(1);
    }

    async fn clear(&self) {
        // Both halves, and both are needed: the first stops feeding the line, the
        // second discards what is already sitting in the carrier's buffer. Doing only
        // the first leaves the caller listening to the sentence they talked over for
        // as long as that buffer is deep.
        self.pacer.clear().await;
        let _ = self.control.send(Control::Clear).await;
        self.down.lock().unwrap().abandon();
        // A new generation, and anybody waiting on the abandoned one is released now.
        //
        // Both halves matter. The far end drops pending marks when it is told to abandon
        // its buffer, so a confirmation for this reply will never arrive and waiting the
        // full grace period for it would delay the interruption by that much. And the
        // interruption is decided on a different task from the one waiting, so the release
        // has to cross between them.
        self.marks.generation.fetch_add(1, Ordering::SeqCst);
        self.began.store(false, Ordering::SeqCst);
        self.marks.changed.notify_waiters();
        metrics::counter!("telephony_clear_total").increment(1);
    }

    fn reply_clock(&self, clock: std::sync::Arc<crate::voice::session::TurnClock>) {
        *self.clock.lock().unwrap() = Some(clock);
    }

    fn wants(&self) -> AudioDelivery {
        AudioDelivery::Samples { rate: SYNTH_RATE }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pace::Outbound;
    use tokio::time::Instant;

    /// `count` samples of a tone, as the bytes a synthesiser would send.
    fn pcm(count: usize) -> Vec<u8> {
        use std::f32::consts::PI;
        (0..count)
            .flat_map(|n| {
                let v = (8000.0 * (2.0 * PI * 440.0 * n as f32 / SYNTH_RATE as f32).sin()) as i16;
                v.to_le_bytes()
            })
            .collect()
    }

    /// However the engine chops the reply up, the line hears the same audio. A chunk
    /// that ends mid-sample is the case that matters: read from the wrong byte, every
    /// sample afterwards is two halves of two different ones.
    #[test]
    fn a_chunk_that_splits_a_sample_changes_nothing() {
        let whole = pcm(2400);
        let at_once = Downlink::new(Wire::Mulaw).feed(&whole);

        // 961 is odd, so it ends half way through a sample.
        let mut split = Downlink::new(Wire::Mulaw);
        let mut framed = split.feed(&whole[..961]);
        framed.extend(split.feed(&whole[961..]));

        assert_eq!(at_once, framed, "the split changed the audio");
        assert!(!at_once.is_empty(), "the fixture produced no frames at all");
    }

    /// The same property across many small chunks, which is what a slow engine
    /// actually looks like.
    #[test]
    fn many_small_chunks_are_the_same_as_one() {
        let whole = pcm(2400);
        let at_once = Downlink::new(Wire::Mulaw).feed(&whole);

        let mut d = Downlink::new(Wire::Mulaw);
        let mut framed = Vec::new();
        for chunk in whole.chunks(37) {
            framed.extend(d.feed(chunk));
        }
        assert_eq!(at_once, framed);
    }

    /// Every frame is exactly one frame, whatever arrives.
    #[test]
    fn frames_are_always_whole() {
        let mut d = Downlink::new(Wire::Mulaw);
        let frames = d.feed(&pcm(2400));
        assert!(frames.iter().all(|f| f.len() == MULAW_FRAME_BYTES));
        // 2400 samples at 24 kHz is 800 at 8 kHz, so five whole frames and no more.
        assert_eq!(frames.len(), 5);
    }

    /// The other wire carries the same twenty milliseconds as raw samples, so a frame is
    /// twice the bytes and the same length of time. Sent at the wrong size this is not an
    /// error anywhere: it is a call of static, which is why the arithmetic is pinned.
    #[test]
    fn the_raw_wire_carries_the_same_frame_in_twice_the_bytes() {
        let mut d = Downlink::new(Wire::Pcm16);
        let frames = d.feed(&pcm(2400));
        assert!(frames.iter().all(|f| f.len() == 320), "a frame is 160 samples of two bytes");
        assert_eq!(frames.len(), 5, "the same five frames the companded wire produces");
        // And the samples are the ones the resampler produced, little-endian, not
        // companded: a mu-law byte where a sample half belongs is inaudible rubbish.
        let mut companded = Downlink::new(Wire::Mulaw);
        let same = companded.feed(&pcm(2400));
        let from_raw: Vec<i16> =
            frames[0].chunks_exact(2).map(|p| i16::from_le_bytes([p[0], p[1]])).collect();
        let from_companded = codec::decode(&same[0]);
        assert_eq!(from_raw.len(), from_companded.len(), "the same number of samples either way");
        // Companding loses precision, so the two agree in shape rather than exactly.
        for (a, b) in from_raw.iter().zip(from_companded.iter()) {
            assert!((*a as i32 - *b as i32).abs() < 512, "{a} and {b} are not the same sample");
        }
    }

    /// The tail of a reply is padded to a whole frame, and silence is not the same byte on
    /// the two wires: a mu-law silence byte in a raw frame is a loud click.
    #[test]
    fn the_tail_is_padded_with_the_silence_that_wire_understands() {
        let mut d = Downlink::new(Wire::Pcm16);
        // Less than a frame's worth, so all of it is still held back.
        let _ = d.feed(&pcm(60));
        let tail = d.flush().expect("a part frame is padded out and sent");
        assert_eq!(tail.len(), 320);
        assert!(tail.ends_with(&[0x00, 0x00]), "raw silence is a zero sample");
    }

    /// Abandoning a reply leaves the next one aligned from its first byte, and keeps
    /// the filter history that makes frames join.
    #[test]
    fn abandoning_drops_the_part_frame_but_not_the_filter() {
        let mut d = Downlink::new(Wire::Mulaw);
        // 300 samples at 24 kHz is 100 at 8 kHz: less than one frame, so it is all
        // still held back.
        assert!(d.feed(&pcm(300)).is_empty());
        d.abandon();
        assert!(d.flush().is_none(), "the abandoned part frame is still there");

        // The next reply starts from a frame boundary.
        let after = d.feed(&pcm(2400));
        assert_eq!(after.len(), 5);
        assert!(after.iter().all(|f| f.len() == MULAW_FRAME_BYTES));
    }

    /// A split sample held over from an abandoned reply must not be joined to the
    /// start of the next one: the two are unrelated audio.
    #[test]
    fn abandoning_drops_a_half_sample_too() {
        let mut d = Downlink::new(Wire::Mulaw);
        d.feed(&pcm(300)[..201]); // odd length, so half a sample is held
        d.abandon();
        assert!(d.spare.is_none(), "half a sample of the abandoned reply survived");
    }

    /// The tail of a reply is padded with silence rather than left short, and silence
    /// on a line is not zero.
    #[test]
    fn the_tail_is_padded_with_silence() {
        let mut d = Downlink::new(Wire::Mulaw);
        d.feed(&pcm(300));
        let tail = d.flush().expect("a tail");
        assert_eq!(tail.len(), MULAW_FRAME_BYTES);
        assert_eq!(tail[MULAW_FRAME_BYTES - 1], MULAW_SILENCE);
        assert!(d.flush().is_none(), "nothing left to flush twice");
    }

    /// Nothing in, nothing out.
    #[test]
    fn an_empty_chunk_produces_nothing() {
        assert!(Downlink::new(Wire::Mulaw).feed(&[]).is_empty());
    }

    /// The sink asks for raw samples at the rate the synthesiser produces. Asking for
    /// anything else means silence, because nothing on the reply path resamples.
    #[tokio::test]
    async fn the_line_asks_for_raw_samples() {
        let (out, _rx) = mpsc::channel(8);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, _crx) = mpsc::channel(4);
        let sink = TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true);
        assert_eq!(sink.wants(), AudioDelivery::Samples { rate: 24_000 });
    }

    /// A reply opens with a mark straight after its first audio, so the confirmation of
    /// that mark is the moment the caller began hearing it.
    #[tokio::test(start_paused = true)]
    async fn a_reply_is_marked_at_its_start_and_its_end() {
        let (out, mut rx) = mpsc::channel(512);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, _crx) = mpsc::channel(4);
        let sink = TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true);

        sink.audio(AudioClip { bytes: pcm(2400), mime: "audio/pcm".into(), seq: 0 }).await;
        // Nothing confirms anything here, so the end wait gives up after its grace and the
        // reply still finishes: a carrier that cannot report playback must not wedge a call.
        sink.audio_end().await;

        let mut marks = Vec::new();
        while let Ok(item) = rx.try_recv() {
            if let Outbound::Mark(id) = item {
                marks.push(id);
            }
        }
        assert_eq!(marks.len(), 2, "a reply is marked at both ends: {marks:?}");
        assert_eq!(marks[0].kind, MarkKind::FirstAudio);
        assert_eq!(marks[1].kind, MarkKind::ReplyEnd);
        assert_eq!(marks[0].generation, marks[1].generation, "one reply, one generation");
    }

    /// A carrier that never reports playback costs one grace period for the whole call,
    /// not one per sentence.
    #[tokio::test(start_paused = true)]
    async fn a_silent_carrier_is_given_up_on_once() {
        let (out, mut rx) = mpsc::channel(512);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, _crx) = mpsc::channel(4);
        let sink = TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let first = Instant::now();
        sink.audio(AudioClip { bytes: pcm(480), mime: "audio/pcm".into(), seq: 0 }).await;
        sink.audio_end().await;
        let after_first = Instant::now().duration_since(first);
        assert!(after_first >= MARK_GRACE, "the first reply did not wait at all: {after_first:?}");

        let second = Instant::now();
        sink.audio(AudioClip { bytes: pcm(480), mime: "audio/pcm".into(), seq: 0 }).await;
        sink.audio_end().await;
        let after_second = Instant::now().duration_since(second);
        assert!(
            after_second < MARK_GRACE,
            "the second reply paid the grace period again: {after_second:?}"
        );
        drain.abort();
    }

    /// A confirmation ends the wait immediately, which is the point of asking.
    #[tokio::test(start_paused = true)]
    async fn a_confirmed_reply_does_not_wait_out_the_grace() {
        let (out, mut rx) = mpsc::channel(512);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, _crx) = mpsc::channel(4);
        let sink = std::sync::Arc::new(TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true));

        // Something standing in for the carrier: read what leaves and confirm each mark.
        let echoer = {
            let sink = sink.clone();
            tokio::spawn(async move {
                while let Some(item) = rx.recv().await {
                    if let Outbound::Mark(id) = item {
                        sink.mark_echoed(&id.name()).await;
                    }
                }
            })
        };

        let start = Instant::now();
        sink.audio(AudioClip { bytes: pcm(480), mime: "audio/pcm".into(), seq: 0 }).await;
        sink.audio_end().await;
        assert!(
            Instant::now().duration_since(start) < MARK_GRACE,
            "a confirmed reply still waited out the grace period"
        );
        echoer.abort();
    }

    /// Interrupting must release a wait at once. The far end drops the marks in a buffer it
    /// has been told to abandon, so the confirmation will never come, and waiting the full
    /// grace for it would delay the interruption by exactly that long.
    #[tokio::test(start_paused = true)]
    async fn interrupting_releases_a_wait_immediately() {
        let (out, _rx) = mpsc::channel(1);
        let (pacer, _task) = super::super::pace::spawn(out, 64);
        let (control, _crx) = mpsc::channel(4);
        let sink = std::sync::Arc::new(TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true));

        sink.audio(AudioClip { bytes: pcm(4800), mime: "audio/pcm".into(), seq: 0 }).await;
        let ending = {
            let sink = sink.clone();
            tokio::spawn(async move { sink.audio_end().await })
        };
        tokio::task::yield_now().await;

        let start = Instant::now();
        sink.clear().await;
        tokio::time::timeout(Duration::from_secs(5), ending)
            .await
            .expect("the wait outlived the interruption")
            .expect("the waiting task panicked");
        assert!(
            Instant::now().duration_since(start) < MARK_GRACE,
            "the interruption was delayed by the wait for a confirmation that never comes"
        );
    }

    /// A confirmation about an abandoned reply is not a confirmation about this one.
    #[tokio::test(start_paused = true)]
    async fn a_stale_confirmation_is_ignored() {
        let (out, mut rx) = mpsc::channel(512);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, _crx) = mpsc::channel(4);
        let sink = TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true);

        sink.audio(AudioClip { bytes: pcm(480), mime: "audio/pcm".into(), seq: 0 }).await;
        let opening = loop {
            match rx.recv().await.expect("something left") {
                Outbound::Mark(id) => break id,
                Outbound::Frame(_) => continue,
            }
        };
        sink.clear().await; // a new generation

        // The abandoned reply's confirmation arrives late.
        sink.mark_echoed(&opening.name()).await;
        assert_eq!(
            sink.marks.heard.load(Ordering::SeqCst),
            0,
            "a confirmation from the abandoned reply was accepted"
        );
    }

    /// Interrupting tells the carrier as well as the pacer. Nothing else in the
    /// system can observe this, so it is asserted here or not at all.
    #[tokio::test]
    async fn interrupting_reaches_the_carrier() {
        let (out, _rx) = mpsc::channel(64);
        let (pacer, _task) = super::super::pace::spawn(out, 1);
        let (control, mut crx) = mpsc::channel(4);
        let sink = TelephonySink::new(pacer, control, crate::telephony::Ending::default(), Wire::Mulaw, true);

        sink.audio(AudioClip { bytes: pcm(2400), mime: "audio/pcm".into(), seq: 0 }).await;
        sink.clear().await;

        assert!(matches!(crx.try_recv(), Ok(Control::Clear)), "the carrier was not told");
    }
}
