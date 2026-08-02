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

//! Keeping the sound of a call, on the lines that record.
//!
//! Only where a line has been set to record, and only where its callers have been told
//! so: the notice and the switch are one decision, made elsewhere and honoured here by
//! there being no way to start this without one.
//!
//! Four things decide the shape of everything below.
//!
//! **Two channels, so it is plain who spoke.** The caller on the left, the line on the
//! right. Mixing them would save nothing at all, since it is the same number of bytes
//! either way, and would lose the one thing that makes a recording of a conversation worth
//! listening to.
//!
//! **Kept as the telephone carries it**: eight kilohertz, companded, which is about nine
//! tenths of a megabyte a minute and needs no encoder that is not already here. It is
//! turned back into ordinary samples on the way out, because that is what will play.
//!
//! **Laid out on the clock, not by counting.** Each side hands over what it has when it
//! has it, and neither side is continuous: a caller is silent while the line speaks. So a
//! chunk is placed where its arrival time says it belongs and any gap is filled with
//! silence. Appending instead would have the two sides drift apart within a minute, and
//! the recording would show the assistant answering questions it had not been asked yet.
//!
//! **Nothing here may delay a call.** Both sides hand over through a channel that drops
//! rather than waits, and a write that fails ends the recording rather than the call.

use std::path::PathBuf;

use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::codec::{self, MULAW_SILENCE, TELEPHONY_RATE};

/// Which side of the conversation a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The person who rang.
    Caller,
    /// What this deployment said back.
    Line,
}

/// One piece of audio, and when it arrived.
struct Chunk {
    side: Side,
    /// Milliseconds since the call was answered, which is where this belongs.
    at_ms: u64,
    /// Companded samples, one byte each.
    ulaw: Vec<u8>,
}

/// How many chunks may be waiting before the recorder starts dropping them.
///
/// A frame arrives every twenty milliseconds from each side, so this is seconds of slack.
/// Past it something is badly wrong with the disk, and the answer is to lose audio rather
/// than to hold up a live call.
const QUEUE: usize = 512;

/// How the recording is written: two channels, one byte a sample, at the line's own rate.
const CHANNELS: u16 = 2;
const BYTES_PER_FRAME: usize = CHANNELS as usize; // one companded byte per channel

/// The handle a call holds. Dropping it closes the recording.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Chunk>,
    started: std::time::Instant,
}

impl Recorder {
    /// Hand over what the caller just said.
    pub fn caller(&self, samples: &[i16]) {
        self.push(Side::Caller, codec::encode(samples));
    }

    /// The same, where the transport already carries companded audio: kept exactly as it
    /// arrived, since that is the form it is stored in.
    pub fn caller_ulaw(&self, ulaw: Vec<u8>) {
        self.push(Side::Caller, ulaw);
    }

    /// Hand over what the line just said, already companded.
    pub fn line_ulaw(&self, ulaw: Vec<u8>) {
        self.push(Side::Line, ulaw);
    }

    /// Hand over what the line just said, as samples.
    pub fn line(&self, samples: &[i16]) {
        self.push(Side::Line, codec::encode(samples));
    }

    fn push(&self, side: Side, ulaw: Vec<u8>) {
        if ulaw.is_empty() {
            return;
        }
        let at_ms = self.started.elapsed().as_millis() as u64;
        // Never waits. A recorder that cannot keep up loses audio, which is a worse
        // recording; a recorder that blocked would be a worse call.
        let _ = self.tx.try_send(Chunk { side, at_ms, ulaw });
    }
}

/// What became of a recording, once the call is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// Where it is, relative to the recordings directory.
    pub path: String,
    pub bytes: u64,
    pub seconds: u32,
    /// Chunks that arrived faster than they could be written, and were lost.
    pub dropped: u64,
    /// The recording did not survive: a write failed part-way through, or nothing could
    /// be written at all.
    pub failed: bool,
}

/// Where a call's recording goes, relative to the recordings directory.
///
/// One file per call, named after it, so the row and the file find each other without a
/// second lookup and an orphan is recognisable on sight.
pub fn relative_path(call_id: uuid::Uuid) -> String {
    format!("{call_id}.wav")
}

/// Start recording a call.
///
/// Returns the handle both sides hand audio to, and a task that finishes the file. The
/// recording ends when every handle has been dropped, which happens when the call does.
pub async fn start(dir: &str, call_id: uuid::Uuid) -> Option<(Recorder, tokio::task::JoinHandle<Finished>)> {
    let rel = relative_path(call_id);
    let abs = crate::storage::resolve_file(dir, &rel);
    if let Some(parent) = abs.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!(error = %e, "could not make room for a call recording");
            return None;
        }
    }
    let file = match tokio::fs::File::create(&abs).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, path = %abs.display(), "could not start a call recording");
            return None;
        }
    };
    let (tx, rx) = mpsc::channel::<Chunk>(QUEUE);
    let started = std::time::Instant::now();
    let task = tokio::spawn(write_it(file, abs, rel, rx));
    Some((Recorder { tx, started }, task))
}

/// Take chunks until the call is over, laying each where its time says it belongs.
async fn write_it(
    mut file: tokio::fs::File,
    abs: PathBuf,
    rel: String,
    mut rx: mpsc::Receiver<Chunk>,
) -> Finished {
    let mut out = Finished { path: rel, bytes: 0, seconds: 0, dropped: 0, failed: false };
    if file.write_all(&header(0)).await.is_err() {
        out.failed = true;
        return out;
    }

    // Where each side has been written up to, in samples since the call was answered, and
    // the audio that has been written for the frames beyond the shorter side. The file is
    // interleaved, so nothing can be written for a moment until both sides are known:
    // what is held here is the part of the timeline that is still open.
    let mut base_samples: u64 = 0; // the first sample still in `pending`
    let mut pending: Vec<[u8; BYTES_PER_FRAME]> = Vec::new();
    let mut written_frames: u64 = 0;
    // How far each side has been given audio for. The shorter of the two is how far the
    // file can be written to: past it, one side is still to arrive.
    let mut have: [u64; 2] = [0, 0];

    while let Some(chunk) = rx.recv().await {
        let idx = match chunk.side {
            Side::Caller => 0,
            Side::Line => 1,
        };
        let at = ms_to_samples(chunk.at_ms);
        // Where this chunk starts: its own time, or where this side had got to, whichever
        // is later. A chunk cannot rewrite audio already on the disk, and one that arrives
        // a little early must not overlap the one before it.
        let start = at.max(have[idx]).max(base_samples);
        let end = start + chunk.ulaw.len() as u64;
        let need = (end - base_samples) as usize;
        if pending.len() < need {
            pending.resize(need, [MULAW_SILENCE; BYTES_PER_FRAME]);
        }
        for (n, b) in chunk.ulaw.iter().enumerate() {
            let at = (start - base_samples) as usize + n;
            pending[at][idx] = *b;
        }
        have[idx] = end;

        // Everything both sides have now been heard for can go to the disk.
        let settled = have[0].min(have[1]);
        if settled > base_samples {
            let take = (settled - base_samples) as usize;
            let mut buf = Vec::with_capacity(take * BYTES_PER_FRAME);
            for frame in pending.drain(..take) {
                buf.extend_from_slice(&frame);
            }
            if file.write_all(&buf).await.is_err() {
                out.failed = true;
                break;
            }
            written_frames += take as u64;
            base_samples = settled;
        }
    }

    // Whatever one side heard after the other stopped: written rather than thrown away,
    // because the end of a call is usually the line speaking to somebody who has already
    // said goodbye.
    if !out.failed && !pending.is_empty() {
        let mut buf = Vec::with_capacity(pending.len() * BYTES_PER_FRAME);
        for frame in pending.drain(..) {
            buf.extend_from_slice(&frame);
        }
        if file.write_all(&buf).await.is_err() {
            out.failed = true;
        } else {
            written_frames += (buf.len() / BYTES_PER_FRAME) as u64;
        }
    }

    // The header could not be written first, because the length was not known then. Go
    // back and put it right, which is the ordinary way of writing this format.
    let data_bytes = written_frames * BYTES_PER_FRAME as u64;
    let patched = file
        .seek(std::io::SeekFrom::Start(0))
        .await
        .and(Ok(()))
        .and(Ok(header(data_bytes as u32)));
    match patched {
        Ok(h) => {
            if file.write_all(&h).await.is_err() || file.flush().await.is_err() {
                out.failed = true;
            }
        }
        Err(_) => out.failed = true,
    }
    let _ = file.sync_all().await;

    out.bytes = HEADER_BYTES as u64 + data_bytes;
    out.seconds = (written_frames / TELEPHONY_RATE as u64) as u32;
    if out.failed || written_frames == 0 {
        // A recording of nothing is not a recording. Taken away rather than left as an
        // empty file somebody would later try to play.
        let _ = tokio::fs::remove_file(&abs).await;
        out.failed = true;
    }
    out
}

/// Milliseconds since the call started, as a sample offset at the line's rate.
fn ms_to_samples(ms: u64) -> u64 {
    ms * TELEPHONY_RATE as u64 / 1000
}

/// The length of the header written before any audio.
pub const HEADER_BYTES: usize = 58;

/// A two-channel companded header, with `data_bytes` of audio behind it.
///
/// Companded audio is not the simple form of this format: it needs the longer format
/// chunk and an empty fact chunk, which is why this is written out here rather than
/// borrowed from the one that writes plain samples for recognition.
fn header(data_bytes: u32) -> Vec<u8> {
    let rate = TELEPHONY_RATE;
    let bits: u16 = 8;
    let block_align = CHANNELS * (bits / 8);
    let byte_rate = rate * block_align as u32;
    // RIFF size = everything after the first eight bytes.
    let riff_len = (HEADER_BYTES as u32 - 8) + data_bytes;
    let mut out = Vec::with_capacity(HEADER_BYTES + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&18u32.to_le_bytes()); // a companded format chunk is 18 bytes
    out.extend_from_slice(&7u16.to_le_bytes()); // 7 = mu-law
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // no extra format bytes
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(data_bytes / CHANNELS as u32).to_le_bytes()); // samples a channel
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_BYTES);
    out
}

/// Turn a stored recording into ordinary samples, for something that has to play it.
///
/// Kept companded on the disk because that is what a telephone carries and it is half the
/// size; turned back here because a companded file is not something every player will
/// open, and a recording nobody can listen to is not a recording.
pub fn to_pcm_wav(stored: &[u8]) -> Option<Vec<u8>> {
    if stored.len() < HEADER_BYTES || &stored[..4] != b"RIFF" {
        return None;
    }
    let samples = codec::decode(&stored[HEADER_BYTES..]);
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let data_len = data.len() as u32;
    let bits: u16 = 16;
    let block_align = CHANNELS * (bits / 8);
    let byte_rate = TELEPHONY_RATE * block_align as u32;
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = plain samples
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&TELEPHONY_RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&data);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, loud: bool) -> Vec<i16> {
        (0..n).map(|i| if loud { ((i % 32) as i16 - 16) * 400 } else { 0 }).collect()
    }

    /// Read a written recording back as two channels of samples.
    fn read_back(bytes: &[u8]) -> (Vec<i16>, Vec<i16>) {
        let all = codec::decode(&bytes[HEADER_BYTES..]);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for pair in all.chunks_exact(2) {
            left.push(pair[0]);
            right.push(pair[1]);
        }
        (left, right)
    }

    async fn record(dir: &tempfile::TempDir, chunks: Vec<(Side, u64, Vec<i16>)>) -> (Finished, Vec<u8>) {
        let id = uuid::Uuid::now_v7();
        let path = dir.path().to_string_lossy().to_string();
        let (rec, task) = start(&path, id).await.expect("a recording starts");
        // Placed by the time each chunk claims rather than by when this test manages to
        // send it, which is what the recorder is being asked to do.
        for (side, at_ms, samples) in chunks {
            let ulaw = codec::encode(&samples);
            rec.tx
                .send(Chunk { side, at_ms, ulaw })
                .await
                .expect("the recorder takes it");
        }
        drop(rec);
        let done = task.await.expect("the recorder finishes");
        let bytes = std::fs::read(dir.path().join(relative_path(id))).unwrap_or_default();
        (done, bytes)
    }

    /// The two sides land in their own channels, at the moment each claims, and neither
    /// pushes the other along.
    #[tokio::test]
    async fn each_side_lands_in_its_own_channel_at_its_own_moment() {
        let dir = tempfile::tempdir().unwrap();
        // The caller speaks for the first 100 ms; the line answers at 200 ms.
        let (done, bytes) = record(
            &dir,
            vec![
                (Side::Caller, 0, tone(800, true)),
                (Side::Line, 200, tone(800, true)),
            ],
        )
        .await;
        assert!(!done.failed, "{done:?}");
        let (left, right) = read_back(&bytes);
        assert_eq!(left.len(), right.len(), "the two channels are the same length");
        // 200 ms is 1600 samples: the caller's audio is at the start of the left channel
        // and the line's begins a fifth of a second in on the right.
        assert!(left[..800].iter().any(|s| s.abs() > 1000), "the caller is not at the start");
        assert!(right[..800].iter().all(|s| s.abs() < 1000), "the line spoke before it did");
        assert!(right[1600..2400].iter().any(|s| s.abs() > 1000), "the line is not where it spoke");
        assert!(left[1600..2400].iter().all(|s| s.abs() < 1000), "the caller is still speaking");
    }

    /// Both at once is the case a mixed recording cannot show, and the reason for two
    /// channels.
    #[tokio::test]
    async fn both_speaking_at_once_occupy_the_same_moment() {
        let dir = tempfile::tempdir().unwrap();
        let (done, bytes) = record(
            &dir,
            vec![(Side::Caller, 0, tone(800, true)), (Side::Line, 0, tone(800, true))],
        )
        .await;
        assert!(!done.failed);
        let (left, right) = read_back(&bytes);
        assert!(left[..800].iter().any(|s| s.abs() > 1000));
        assert!(right[..800].iter().any(|s| s.abs() > 1000));
        assert_eq!(left.len(), 800, "neither side was pushed along by the other");
    }

    /// A gap is silence rather than a shift. Appending instead is the failure this whole
    /// design exists to avoid: the reply would creep earlier through the call until the
    /// recording showed the line answering before the caller had spoken.
    #[tokio::test]
    async fn a_gap_becomes_silence_and_nothing_slides_forward() {
        let dir = tempfile::tempdir().unwrap();
        let (done, bytes) = record(
            &dir,
            vec![
                (Side::Caller, 0, tone(160, true)),
                // Nothing from anybody for a second, then the caller again.
                (Side::Caller, 1_000, tone(160, true)),
                (Side::Line, 1_200, tone(160, true)),
            ],
        )
        .await;
        assert!(!done.failed);
        let (left, right) = read_back(&bytes);
        // A second is 8000 samples: the second burst is there and not at 160.
        assert!(left[..160].iter().any(|s| s.abs() > 1000));
        assert!(left[200..7_900].iter().all(|s| s.abs() < 1000), "the gap is not silent");
        assert!(left[8_000..8_160].iter().any(|s| s.abs() > 1000), "the second burst moved");
        assert!(right[9_600..9_760].iter().any(|s| s.abs() > 1000), "the reply moved");
    }

    /// What one side said after the other stopped is kept: the end of a call is usually
    /// the line still speaking to somebody who has said goodbye.
    #[tokio::test]
    async fn the_tail_after_one_side_stops_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (done, bytes) = record(
            &dir,
            vec![(Side::Caller, 0, tone(160, true)), (Side::Line, 0, tone(1_600, true))],
        )
        .await;
        assert!(!done.failed);
        let (left, right) = read_back(&bytes);
        assert_eq!(left.len(), 1_600, "the longer side decides the length");
        assert!(right[1_000..1_600].iter().any(|s| s.abs() > 1000), "the tail was thrown away");
    }

    /// The header says what is behind it, and the arithmetic of what a minute costs is
    /// pinned rather than assumed.
    #[tokio::test]
    async fn the_header_matches_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let (done, bytes) = record(&dir, vec![(Side::Caller, 0, tone(8_000, true)), (Side::Line, 0, tone(8_000, true))]).await;
        assert!(!done.failed);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // Companded, two channels, at the line's own rate.
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 7, "not companded");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2, "not two channels");
        assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 8_000);
        let data_len = u32::from_le_bytes([
            bytes[HEADER_BYTES - 4],
            bytes[HEADER_BYTES - 3],
            bytes[HEADER_BYTES - 2],
            bytes[HEADER_BYTES - 1],
        ]);
        assert_eq!(data_len as usize, bytes.len() - HEADER_BYTES, "the header lies about the length");
        assert_eq!(done.bytes as usize, bytes.len());
        assert_eq!(done.seconds, 1, "one second of audio either side");
        // One second is 16 kB, so a minute is a little under a megabyte.
        assert_eq!(data_len, 16_000);
    }

    /// A recording with nothing in it is not left behind for somebody to try to play.
    #[tokio::test]
    async fn a_recording_of_nothing_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::now_v7();
        let (rec, task) = start(&dir.path().to_string_lossy(), id).await.expect("starts");
        drop(rec);
        let done = task.await.expect("finishes");
        assert!(done.failed, "an empty recording should not read as a good one");
        assert!(!dir.path().join(relative_path(id)).exists(), "an empty file was left behind");
    }

    /// What is stored is companded and what is served is not, because a companded file is
    /// not something every player will open.
    #[test]
    fn a_stored_recording_is_served_as_ordinary_samples() {
        let mut stored = header(4);
        stored.extend_from_slice(&[MULAW_SILENCE, MULAW_SILENCE, 0x00, 0x00]);
        let served = to_pcm_wav(&stored).expect("converts");
        assert_eq!(&served[..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([served[20], served[21]]), 1, "not plain samples");
        assert_eq!(u16::from_le_bytes([served[22], served[23]]), 2, "channels were lost");
        assert_eq!(u32::from_le_bytes([served[24], served[25], served[26], served[27]]), 8_000);
        // Two frames of two channels, two bytes a sample.
        assert_eq!(served.len(), 44 + 8);
        assert!(to_pcm_wav(b"not a recording").is_none());
    }

    #[test]
    fn a_recording_is_named_after_its_call() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(relative_path(id), format!("{id}.wav"));
        assert!(!relative_path(id).contains('/'), "one flat name, so an orphan is obvious");
    }
}
