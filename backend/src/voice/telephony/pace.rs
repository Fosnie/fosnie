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

//! Releasing reply audio at the rate a telephone line consumes it.
//!
//! A synthesiser produces a sentence in a burst, far faster than it takes to say.
//! A line takes one frame every 20 ms and no faster. Pushing the burst straight
//! down the line does not make the reply arrive sooner: it fills a buffer somewhere
//! downstream, and then everything that depends on knowing what the speaker has
//! actually heard is wrong. Interrupting is the case that matters. Cutting the reply
//! when three seconds of it are already sitting in a buffer cuts audio the speaker
//! has not reached yet, so they hear the assistant carry on talking over them.
//!
//! So the frames are held here, and released on a clock.
//!
//! Emptying the queue is only half of an interruption: whatever is already
//! downstream is the transport's own to discard, by whatever means it has.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use super::codec::FRAME_MS;

/// How much audio to gather before starting to release it. Enough that a late
/// frame does not immediately become a gap in the middle of a word.
pub const DEFAULT_PREBUFFER: usize = 3;

enum PacerCmd {
    Push(Vec<u8>),
    Mark(MarkId),
    Clear,
    Close,
}

/// One thing to put on the line, in the order the line will hear it.
///
/// A mark travels with the audio rather than beside it, because what it means is "you have
/// now played everything before this". Sent by any faster route it would arrive before the
/// audio it refers to and mean nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    Frame(Vec<u8>),
    Mark(MarkId),
}

/// Which point in which reply a mark stands for.
///
/// `generation` counts replies rather than naming them, so the only question an echo has to
/// answer is whether it is stale, and that is one comparison. A reply that was interrupted
/// bumps it too, which is what stops the abandoned reply's echo from being read as the new
/// one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkId {
    pub generation: u64,
    pub kind: MarkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    /// The first audio of a reply: when the speaker began hearing it.
    FirstAudio,
    /// The last: when they finished hearing it.
    ReplyEnd,
}

impl MarkId {
    /// The name the far end echoes back.
    pub fn name(&self) -> String {
        let kind = match self.kind {
            MarkKind::FirstAudio => "begin",
            MarkKind::ReplyEnd => "end",
        };
        format!("r{}-{kind}", self.generation)
    }

    /// Read a name back. `None` for anything this process did not write, which is the
    /// right answer for a mark somebody else's software put on the line.
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix('r')?;
        let (generation, kind) = rest.split_once('-')?;
        let kind = match kind {
            "begin" => MarkKind::FirstAudio,
            "end" => MarkKind::ReplyEnd,
            _ => return None,
        };
        Some(MarkId { generation: generation.parse().ok()?, kind })
    }
}

/// How much audio has been handed over and not yet released onto the line.
///
/// Shared rather than reported back, because the interesting question is asked from the
/// other side: whoever queued a reply needs to know when the line has actually finished
/// playing it, and that is not the same moment as when the last frame was handed over.
struct Depth {
    frames: AtomicUsize,
    empty: Notify,
}

impl Depth {
    fn add(&self) {
        self.frames.fetch_add(1, Ordering::SeqCst);
    }

    fn took_one(&self) {
        if self.frames.fetch_sub(1, Ordering::SeqCst) <= 1 {
            self.empty.notify_waiters();
        }
    }

    fn emptied(&self) {
        self.frames.store(0, Ordering::SeqCst);
        self.empty.notify_waiters();
    }
}

/// The producer's end of a running pacer.
pub struct PacerHandle {
    tx: mpsc::Sender<PacerCmd>,
    depth: Arc<Depth>,
}

impl PacerHandle {
    /// Hand over one frame to be released in its turn.
    pub async fn push(&self, frame: Vec<u8>) {
        // Counted here rather than where it is received, because the count answers a
        // question the caller asks: "is the line still playing what I gave it?" A count
        // kept on the far side would read as zero for as long as the frame was in
        // transit, and anybody waiting on it would be told the reply had finished
        // before it had started.
        self.depth.add();
        if self.tx.send(PacerCmd::Push(frame)).await.is_err() {
            self.depth.took_one();
        }
    }

    /// Discard everything queued and not yet released, and gather a fresh prebuffer
    /// before releasing anything again.
    pub async fn clear(&self) {
        // Zeroed on this side too, so that anybody waiting for the abandoned reply to
        // finish is released now rather than when the pacer gets round to the message.
        self.depth.emptied();
        let _ = self.tx.send(PacerCmd::Clear).await;
    }

    /// Ask the far end to say when it has played everything queued before this.
    ///
    /// Queued with the audio, so it keeps its place in it.
    pub async fn mark(&self, id: MarkId) {
        let _ = self.tx.send(PacerCmd::Mark(id)).await;
    }

    /// Stop. Anything still queued is dropped, and the output closes.
    pub async fn close(&self) {
        let _ = self.tx.send(PacerCmd::Close).await;
    }

    /// Wait until everything handed over has been released onto the line.
    ///
    /// This is what makes "finished speaking" mean what it says. Synthesis finishes
    /// long before a line has played the result, and a session that treated the two as
    /// the same moment would consider itself finished while the caller is still
    /// listening: speech during the rest of the reply would be taken for a new question
    /// rather than an interruption, and the caller would be talked over by an answer to
    /// something they had not asked.
    ///
    /// Returns as soon as the queue empties for any reason, including being discarded
    /// by an interruption or the line going away, so it cannot outlive the audio it is
    /// waiting on.
    pub async fn wait_drained(&self) {
        loop {
            // Registered before the count is read, so a frame released in between wakes
            // this rather than being missed.
            let waiting = self.depth.empty.notified();
            if self.depth.frames.load(Ordering::SeqCst) == 0 || self.tx.is_closed() {
                return;
            }
            waiting.await;
        }
    }
}

/// Start a pacer that releases one frame every [`FRAME_MS`] into `out`, beginning
/// once `prebuffer` frames have arrived.
///
/// The task ends when [`PacerHandle::close`] is called, when the handle is dropped,
/// or when `out` is closed.
pub fn spawn(out: mpsc::Sender<Outbound>, prebuffer: usize) -> (PacerHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(256);
    let depth = Arc::new(Depth { frames: AtomicUsize::new(0), empty: Notify::new() });
    let task = tokio::spawn(run(rx, out, prebuffer.max(1), depth.clone()));
    (PacerHandle { tx, depth }, task)
}

async fn run(
    mut rx: mpsc::Receiver<PacerCmd>,
    out: mpsc::Sender<Outbound>,
    prebuffer: usize,
    depth: Arc<Depth>,
) {
    let mut queue: VecDeque<Outbound> = VecDeque::new();
    // Counted apart from the queue's length, because only audio takes time to play. A
    // queue of nothing but marks holds no audio, so it must not be mistaken for a filled
    // prebuffer.
    let mut frames_queued = 0usize;
    // Not releasing yet: the prebuffer has still to fill.
    let mut flowing = false;
    let mut ticker = interval(Duration::from_millis(FRAME_MS as u64));
    // The default behaviour after a stall is to fire every tick that was missed in
    // one go, which is precisely the burst this exists to prevent. Skipping them
    // instead would drop audio. Delay keeps every frame and keeps the spacing.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(PacerCmd::Push(frame)) => {
                    queue.push_back(Outbound::Frame(frame));
                    frames_queued += 1;
                    if frames_queued >= prebuffer {
                        flowing = true;
                    }
                }
                Some(PacerCmd::Mark(id)) => {
                    queue.push_back(Outbound::Mark(id));
                }
                Some(PacerCmd::Clear) => {
                    queue.clear();
                    frames_queued = 0;
                    flowing = false;
                    depth.emptied();
                }
                // Closed explicitly, or the producer went away.
                Some(PacerCmd::Close) | None => break,
            },
            _ = ticker.tick(), if flowing => {
                // One tick releases one *frame*. Marks in front of it go out with it and
                // cost nothing, because a mark is a position in the audio rather than a
                // piece of it: spending a tick on one would insert silence and put the
                // whole reply out of step with the clock releasing it.
                let mut sent_frame = false;
                while !sent_frame {
                    match queue.pop_front() {
                        Some(Outbound::Mark(id)) => {
                            if out.send(Outbound::Mark(id)).await.is_err() {
                                return;
                            }
                        }
                        Some(Outbound::Frame(frame)) => {
                            if out.send(Outbound::Frame(frame)).await.is_err() {
                                depth.emptied();
                                return; // the transport is gone
                            }
                            frames_queued = frames_queued.saturating_sub(1);
                            depth.took_one();
                            sent_frame = true;
                            // Any marks sitting directly behind this frame go with it. A
                            // mark means "you have played everything before this", so the
                            // frame it follows is exactly where it belongs; held back for
                            // the next tick, the last mark of a reply would wait on audio
                            // that is never coming.
                            while let Some(Outbound::Mark(_)) = queue.front() {
                                let Some(item) = queue.pop_front() else { break };
                                if out.send(item).await.is_err() {
                                    depth.emptied();
                                    return;
                                }
                            }
                        }
                        // Ran dry mid-reply. Wait for the prebuffer to refill rather than
                        // release each frame the moment it lands: what to put on the line
                        // in the meantime is the transport's decision, not this one's.
                        None => {
                            flowing = false;
                            break;
                        }
                    }
                }
            },
        }
    }
    // However this ended, nothing more will be released. Anybody waiting for the line
    // to finish has to be let go, or a closed call would hold a turn open for ever.
    depth.emptied();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    /// Every test here runs on a paused clock, so the waiting is virtual: the suite
    /// proves the spacing in milliseconds without spending any.
    fn frame(n: u8) -> Vec<u8> {
        vec![n; 4]
    }

    /// The audio out of a released item. Panics on a mark, so a test that expected audio
    /// and got a position marker says so rather than comparing something meaningless.
    fn frame_out(item: Outbound) -> Vec<u8> {
        match item {
            Outbound::Frame(bytes) => bytes,
            Outbound::Mark(id) => panic!("expected audio, got the mark {}", id.name()),
        }
    }

    /// The point of the whole file: frames leave 20 ms apart, however fast they came.
    #[tokio::test(start_paused = true)]
    async fn frames_leave_exactly_one_frame_apart() {
        let (out_tx, mut out) = mpsc::channel(16);
        let (pacer, _task) = spawn(out_tx, DEFAULT_PREBUFFER);
        for n in 0..5 {
            pacer.push(frame(n)).await;
        }

        let mut previous: Option<Instant> = None;
        for n in 0..5u8 {
            let got = frame_out(out.recv().await.expect("a frame"));
            let now = Instant::now();
            assert_eq!(got, frame(n), "out of order");
            if let Some(p) = previous {
                assert_eq!(
                    now.duration_since(p),
                    Duration::from_millis(FRAME_MS as u64),
                    "frame {n} is not one frame after the last"
                );
            }
            previous = Some(now);
        }
    }

    /// A hundred frames handed over at once still take a hundred frames' worth of
    /// time to leave, and all of them leave.
    #[tokio::test(start_paused = true)]
    async fn a_burst_in_is_still_paced_out() {
        let (out_tx, mut out) = mpsc::channel(256);
        let (pacer, _task) = spawn(out_tx, DEFAULT_PREBUFFER);
        for n in 0..100u32 {
            pacer.push(vec![n as u8]).await;
        }

        let start = Instant::now();
        let mut count = 0;
        while count < 100 {
            frame_out(out.recv().await.expect("a frame"));
            count += 1;
        }
        // The first leaves at once and the rest follow, so the last is 99 frames on.
        assert_eq!(
            Instant::now().duration_since(start),
            Duration::from_millis(99 * FRAME_MS as u64),
            "a hundred frames left in the wrong span of time"
        );
    }

    /// Nothing is released until there is enough audio to release smoothly.
    #[tokio::test(start_paused = true)]
    async fn nothing_leaves_before_the_prebuffer_is_full() {
        let (out_tx, mut out) = mpsc::channel(16);
        let (pacer, _task) = spawn(out_tx, 3);
        pacer.push(frame(0)).await;
        pacer.push(frame(1)).await;

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(out.try_recv().is_err(), "two frames were released on a prebuffer of three");

        pacer.push(frame(2)).await;
        assert_eq!(frame_out(out.recv().await.expect("a frame")), frame(0), "the third push starts the flow");
    }

    /// Interruption. What is queued has to go, or the speaker keeps hearing the
    /// reply they talked over for as long as the queue is deep.
    #[tokio::test(start_paused = true)]
    async fn clearing_discards_what_has_not_been_released() {
        // One frame of capacity, so at most one frame can already be in the hands of
        // the transport when the interruption arrives. Everything beyond that is
        // still here to be discarded.
        let (out_tx, mut out) = mpsc::channel(1);
        let (pacer, _task) = spawn(out_tx, DEFAULT_PREBUFFER);
        for n in 0..10u8 {
            pacer.push(frame(n)).await;
        }
        assert_eq!(frame_out(out.recv().await.expect("a frame")), frame(0));
        assert_eq!(frame_out(out.recv().await.expect("a frame")), frame(1));

        pacer.clear().await;
        for n in 10..13u8 {
            pacer.push(frame(n)).await;
        }

        let mut old = 0usize;
        let mut fresh = Vec::new();
        while fresh.len() < 3 {
            let got = frame_out(out.recv().await.expect("a frame"));
            if got[0] >= 10 {
                fresh.push(got[0]);
            } else {
                assert!(fresh.is_empty(), "an abandoned frame arrived after a fresh one");
                old += 1;
            }
        }
        assert_eq!(fresh, vec![10, 11, 12], "the fresh reply is not intact");
        assert!(old <= 1, "{old} abandoned frames were still released");
    }

    /// Closing ends the task and closes the line's end of the channel, so whatever
    /// is writing to the carrier learns the call is over rather than waiting.
    #[tokio::test(start_paused = true)]
    async fn closing_ends_it() {
        let (out_tx, mut out) = mpsc::channel(16);
        let (pacer, task) = spawn(out_tx, DEFAULT_PREBUFFER);
        pacer.close().await;
        task.await.expect("the pacer stopped cleanly");
        assert!(out.recv().await.is_none(), "the output must close with the pacer");
    }

    /// Waiting for the line means waiting for the audio to be played, not for it to be
    /// handed over. The difference is the whole reply.
    #[tokio::test(start_paused = true)]
    async fn waiting_for_the_line_waits_for_the_audio_to_be_played() {
        let (out_tx, mut out) = mpsc::channel(64);
        let (pacer, _task) = spawn(out_tx, 1);
        for n in 0..10 {
            pacer.push(frame(n)).await;
        }
        // Something has to be taking the frames off the line, as a carrier's socket does.
        let drain = tokio::spawn(async move { while out.recv().await.is_some() {} });

        let start = Instant::now();
        pacer.wait_drained().await;
        let waited = Instant::now().duration_since(start);
        assert_eq!(
            waited,
            Duration::from_millis(9 * FRAME_MS as u64),
            "waiting returned before the line had played the reply"
        );
        drain.abort();
    }

    /// Nothing queued means nothing to wait for.
    #[tokio::test(start_paused = true)]
    async fn waiting_with_nothing_queued_returns_at_once() {
        let (out_tx, _out) = mpsc::channel(4);
        let (pacer, _task) = spawn(out_tx, 1);
        let start = Instant::now();
        pacer.wait_drained().await;
        assert_eq!(Instant::now().duration_since(start), Duration::ZERO);
    }

    /// An interruption throws the queue away, so whoever was waiting for it to play is
    /// released rather than left waiting for audio that will never be sent.
    #[tokio::test(start_paused = true)]
    async fn an_interruption_releases_the_wait() {
        let (out_tx, _out) = mpsc::channel(1);
        let (pacer, _task) = spawn(out_tx, 8);
        for n in 0..6 {
            pacer.push(frame(n)).await;
        }
        let handle = Arc::new(pacer);
        let waiter = {
            let p = handle.clone();
            tokio::spawn(async move { p.wait_drained().await })
        };
        handle.clear().await;
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the wait was never released")
            .expect("the waiting task panicked");
    }

    /// And so does the line going away, or a call whose socket died would hold its turn
    /// open for the life of the process.
    #[tokio::test(start_paused = true)]
    async fn the_line_going_away_releases_the_wait() {
        let (out_tx, out) = mpsc::channel(1);
        let (pacer, _task) = spawn(out_tx, 1);
        for n in 0..6 {
            pacer.push(frame(n)).await;
        }
        drop(out); // the carrier's socket has gone
        tokio::time::timeout(Duration::from_secs(5), pacer.wait_drained())
            .await
            .expect("the wait outlived the line");
    }

    fn begin(generation: u64) -> MarkId {
        MarkId { generation, kind: MarkKind::FirstAudio }
    }

    /// A mark is a position in the audio, not a piece of it, so it must not consume a
    /// release slot. Two marks a reply that each cost a tick would insert 40 ms of silence
    /// and put the whole reply out of step with the clock releasing it.
    #[tokio::test(start_paused = true)]
    async fn a_mark_does_not_spend_a_tick() {
        let (out_tx, mut out) = mpsc::channel(512);
        let (pacer, _task) = spawn(out_tx, 1);
        for n in 0..100u32 {
            pacer.push(vec![n as u8]).await;
            pacer.mark(begin(n as u64)).await;
        }

        let start = Instant::now();
        let mut frames = 0;
        while frames < 100 {
            if let Outbound::Frame(_) = out.recv().await.expect("something") {
                frames += 1;
            }
        }
        // Exactly the span a hundred frames take with no marks at all.
        assert_eq!(
            Instant::now().duration_since(start),
            Duration::from_millis(99 * FRAME_MS as u64),
            "the marks stole release slots from the audio"
        );
    }

    /// The prebuffer is there to hold audio, so it has to count audio. A queue of marks
    /// holds none, and must not be mistaken for a filled one.
    #[tokio::test(start_paused = true)]
    async fn the_prebuffer_counts_audio_and_not_marks() {
        let (out_tx, mut out) = mpsc::channel(16);
        let (pacer, _task) = spawn(out_tx, 3);
        pacer.push(frame(0)).await;
        pacer.mark(begin(1)).await;
        pacer.push(frame(1)).await;
        pacer.mark(begin(2)).await;

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(out.try_recv().is_err(), "two frames and two marks started the flow");

        pacer.push(frame(2)).await;
        assert_eq!(frame_out(out.recv().await.expect("a frame")), frame(0));
    }

    /// Waiting for the line to finish means waiting for the audio, and a mark is not
    /// audio. Counting one would have the wait expect something with no duration.
    #[tokio::test(start_paused = true)]
    async fn marks_do_not_lengthen_the_wait_for_the_line() {
        let (out_tx, mut out) = mpsc::channel(64);
        let (pacer, _task) = spawn(out_tx, 1);
        for n in 0..10u8 {
            pacer.push(frame(n)).await;
            pacer.mark(begin(n as u64)).await;
        }
        let drain = tokio::spawn(async move { while out.recv().await.is_some() {} });

        let start = Instant::now();
        pacer.wait_drained().await;
        assert_eq!(
            Instant::now().duration_since(start),
            Duration::from_millis(9 * FRAME_MS as u64),
            "the marks were counted as audio still to play"
        );
        drain.abort();
    }

    /// An interruption throws away queued marks along with the audio, which matches what
    /// the far end does: told to abandon its buffer, it never reports the marks in it.
    #[tokio::test(start_paused = true)]
    async fn clearing_discards_queued_marks_too() {
        let (out_tx, mut out) = mpsc::channel(64);
        let (pacer, _task) = spawn(out_tx, 8);
        for n in 0..4u8 {
            pacer.push(frame(n)).await;
            pacer.mark(begin(n as u64)).await;
        }
        pacer.clear().await;
        pacer.push(frame(9)).await;
        tokio::time::advance(Duration::from_secs(1)).await;

        // Nothing from before the interruption, in either shape.
        while let Ok(item) = out.try_recv() {
            match item {
                Outbound::Frame(bytes) => assert_eq!(bytes, frame(9)),
                Outbound::Mark(id) => panic!("an abandoned mark was sent: {}", id.name()),
            }
        }
    }

    /// A mark's name is written by one end and read by the other, so the pairing is
    /// asserted rather than assumed: a mismatch would silently stop every confirmation
    /// from being recognised.
    #[test]
    fn a_mark_name_survives_the_round_trip() {
        for id in [
            MarkId { generation: 0, kind: MarkKind::FirstAudio },
            MarkId { generation: 7, kind: MarkKind::ReplyEnd },
            MarkId { generation: u64::MAX, kind: MarkKind::FirstAudio },
        ] {
            assert_eq!(MarkId::parse(&id.name()), Some(id), "{}", id.name());
        }
        assert_eq!(MarkId { generation: 3, kind: MarkKind::FirstAudio }.name(), "r3-begin");
        assert_eq!(MarkId { generation: 3, kind: MarkKind::ReplyEnd }.name(), "r3-end");
    }

    /// A name this process did not write is not read as one of ours. Somebody else's
    /// software may be putting marks on the same line, and reading one of those as ours
    /// would confirm a reply that is still playing.
    #[test]
    fn a_name_we_did_not_write_is_not_ours() {
        for junk in ["", "begin", "r-begin", "rx-begin", "r1", "r1-", "r1-middle", "1-begin", "r1-begin-x"] {
            assert_eq!(MarkId::parse(junk), None, "{junk:?} was read as one of ours");
        }
    }

    /// Dropping the producer is the same as closing: a session that went away must
    /// not leave a task ticking for the rest of the process's life.
    #[tokio::test(start_paused = true)]
    async fn dropping_the_handle_ends_it_too() {
        let (out_tx, mut out) = mpsc::channel(16);
        let (pacer, task) = spawn(out_tx, DEFAULT_PREBUFFER);
        drop(pacer);
        task.await.expect("the pacer stopped cleanly");
        assert!(out.recv().await.is_none());
    }
}
