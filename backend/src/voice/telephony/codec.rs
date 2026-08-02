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

//! Turning telephone audio into engine audio, and back.
//!
//! Three jobs, none of which knows about the others' callers:
//!
//! - **G.711 mu-law**, the codec every telephone line speaks. Eight bits a sample
//!   on a logarithmic curve, so quiet passages keep their detail and loud ones lose
//!   some: it is lossy by construction, and a round trip returns something close to
//!   what went in rather than the same thing.
//! - **Resampling.** A line carries 8 kHz. Recognition wants 16 kHz; synthesis
//!   produces 24 kHz, because that is the sample rate of the raw-samples response
//!   format the synthesisers offer. Both conversions have to remove the frequencies
//!   the other rate cannot represent, or what is left folds back into the audible
//!   band as a tone that was never spoken.
//! - **Framing** into the 20 ms blocks a line is carried in.
//!
//! Upsampling does not put back what narrowband lost. It exists so the recognition
//! engine is fed the rate it expects, not to improve the audio.

/// The sample rate of a telephone line.
pub const TELEPHONY_RATE: u32 = 8_000;

/// How much audio one frame on a line carries.
pub const FRAME_MS: u32 = 20;

/// Samples in one frame at [`TELEPHONY_RATE`].
pub const FRAME_SAMPLES: usize = (TELEPHONY_RATE as usize * FRAME_MS as usize) / 1000;

/// Bytes in one mu-law frame: the codec is one byte per sample.
pub const MULAW_FRAME_BYTES: usize = FRAME_SAMPLES;

/// The byte that means silence in mu-law. **Not** zero: zero decodes to full-scale
/// negative, so padding a short frame with zeroes puts a burst of noise on the line.
pub const MULAW_SILENCE: u8 = 0xFF;

// ---------------------------------------------------------------------------
// G.711 mu-law (ITU-T G.711, the complemented form that goes on the wire)
// ---------------------------------------------------------------------------

/// Added before the exponent is taken, so that the smallest magnitudes land in the
/// first segment rather than below it.
const BIAS: i32 = 0x84;
/// The largest magnitude the codec represents, in the 14-bit domain it works in.
const CLIP_14: i32 = 8159;
/// Upper bound of each of the eight segments, in the biased 14-bit domain.
const SEG_END: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

/// Which segment a biased 14-bit magnitude falls in, or 8 if it is past the last.
fn segment(biased: i32) -> usize {
    SEG_END.iter().position(|&end| biased <= end).unwrap_or(8)
}

/// Encode one sample.
pub fn encode_sample(pcm: i16) -> u8 {
    // The codec is defined on 14 bits, so the two least significant bits go first.
    let v = (pcm as i32) >> 2;
    let (mut mag, mask) = if v < 0 { (-v, 0x7Fu8) } else { (v, 0xFFu8) };
    if mag > CLIP_14 {
        mag = CLIP_14;
    }
    let biased = mag + (BIAS >> 2);
    let seg = segment(biased);
    if seg >= 8 {
        // Only reachable for a magnitude the clip above already excluded, but the
        // standard defines the case, so it is written rather than assumed away.
        return 0x7F ^ mask;
    }
    let quantised = ((biased >> (seg + 1)) & 0xF) as u8;
    (((seg as u8) << 4) | quantised) ^ mask
}

/// Decode one sample.
pub fn decode_sample(ulaw: u8) -> i16 {
    let u = !ulaw;
    let mut t = (((u & 0x0F) as i32) << 3) + BIAS;
    t <<= ((u & 0x70) >> 4) as i32;
    let v = if u & 0x80 != 0 { BIAS - t } else { t - BIAS };
    v as i16
}

/// Encode a buffer: one output byte per input sample.
pub fn encode(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().copied().map(encode_sample).collect()
}

/// Decode a buffer: one output sample per input byte.
pub fn decode(ulaw: &[u8]) -> Vec<i16> {
    ulaw.iter().copied().map(decode_sample).collect()
}

// ---------------------------------------------------------------------------
// Resampling
// ---------------------------------------------------------------------------

/// The top of the band a telephone line carries. Everything above it is removed in
/// both directions: on the way in there is nothing there to keep, and on the way out
/// it would fold back into speech as a tone.
const PASSBAND_HZ: f32 = 3_400.0;

/// Filter length. Long enough that the band between the passband and the first
/// frequency that would fold is fully attenuated, short enough to be free.
const FIR_TAPS: usize = 63;

/// A windowed-sinc low pass, normalised so its response at zero frequency is exactly
/// `gain`. Interpolation inserts zeroes and so loses amplitude in proportion to how
/// many; `gain` is where that is put back.
fn low_pass(taps: usize, cutoff_hz: f32, rate_hz: f32, gain: f32) -> Vec<f32> {
    use std::f32::consts::PI;
    let fc = cutoff_hz / rate_hz;
    let m = (taps - 1) as f32;
    let mut h: Vec<f32> = (0..taps)
        .map(|n| {
            let x = n as f32 - m / 2.0;
            let sinc = if x.abs() < 1e-6 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
            let hamming = 0.54 - 0.46 * (2.0 * PI * n as f32 / m).cos();
            sinc * hamming
        })
        .collect();
    let sum: f32 = h.iter().sum();
    for v in h.iter_mut() {
        *v *= gain / sum;
    }
    h
}

fn to_i16(v: f32) -> i16 {
    v.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// A fixed-ratio resampler that keeps its filter history across calls, so
/// consecutive frames join without a step at the seam.
///
/// Feeding the same audio in one buffer or in twenty gives the same samples out.
/// That is the whole reason the state is here rather than inside `process`: a
/// stateless filter starts from silence every frame, and 50 of those a second is an
/// audible click 50 times a second.
pub struct Resampler {
    taps: Vec<f32>,
    /// The `taps.len() - 1` most recent samples at the interpolated rate, which the
    /// next call needs to compute its first outputs.
    hist: Vec<f32>,
    up: usize,
    down: usize,
    /// Which of every `down` interpolated samples to keep next, carried so the
    /// decimation grid does not restart at each frame.
    phase: usize,
}

impl Resampler {
    /// Caller audio, from the line's rate to the rate the recognition engine wants.
    pub fn up_8k_to_16k() -> Self {
        Self::new(2, 1, low_pass(FIR_TAPS, PASSBAND_HZ, 16_000.0, 2.0))
    }

    /// Reply audio, from the rate the synthesiser produces to the line's rate.
    pub fn down_24k_to_8k() -> Self {
        Self::new(1, 3, low_pass(FIR_TAPS, PASSBAND_HZ, 24_000.0, 1.0))
    }

    fn new(up: usize, down: usize, taps: Vec<f32>) -> Self {
        let hist = vec![0.0; taps.len() - 1];
        Self { taps, hist, up, down, phase: 0 }
    }

    /// Resample one buffer, carrying the filter state forward.
    ///
    /// Returns `input.len() * up / down` samples whenever that division is exact,
    /// which it is for a whole number of frames. The output lags the input by the
    /// filter's delay, so the very first call of a session begins with a few
    /// milliseconds of near-silence.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if input.is_empty() {
            return Vec::new();
        }
        let n_hist = self.hist.len();
        let total = input.len() * self.up;
        let mut stream = Vec::with_capacity(n_hist + total);
        stream.extend_from_slice(&self.hist);
        for &s in input {
            stream.push(s as f32);
            for _ in 1..self.up {
                stream.push(0.0);
            }
        }

        let mut out = Vec::with_capacity(total / self.down + 1);
        let mut i = self.phase;
        while i < total {
            let base = n_hist + i;
            let mut acc = 0.0f32;
            for (k, &t) in self.taps.iter().enumerate() {
                acc += t * stream[base - k];
            }
            out.push(to_i16(acc));
            i += self.down;
        }
        self.phase = i - total;

        self.hist.clear();
        self.hist.extend_from_slice(&stream[stream.len() - n_hist..]);
        out
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Cuts a byte stream into fixed-size frames, holding the remainder for next time.
///
/// A synthesiser emits whatever length its network chunks happen to be; a line
/// takes one exact frame at a time.
pub struct Framer {
    buf: Vec<u8>,
    frame_bytes: usize,
}

impl Framer {
    pub fn new(frame_bytes: usize) -> Self {
        assert!(frame_bytes > 0, "a frame has to have a size");
        Self { buf: Vec::with_capacity(frame_bytes * 2), frame_bytes }
    }

    /// Add bytes and take every whole frame that is now available.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(self.buf.len() / self.frame_bytes);
        while self.buf.len() >= self.frame_bytes {
            out.push(self.buf.drain(..self.frame_bytes).collect());
        }
        out
    }

    /// Take the remainder as one frame, padded out with `pad`. `None` when there is
    /// nothing left. For mu-law the pad is [`MULAW_SILENCE`].
    pub fn flush(&mut self, pad: u8) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            return None;
        }
        let mut frame = std::mem::take(&mut self.buf);
        frame.resize(self.frame_bytes, pad);
        Some(frame)
    }

    /// Bytes held back, short of a whole frame.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one exact property a lossy codec has: every code is a fixed point.
    /// Decoding a byte and encoding the result must give that byte back, so the
    /// eight-bit alphabet is closed and no code is unreachable.
    #[test]
    fn every_code_survives_a_trip_through_samples() {
        for u in 0..=255u8 {
            let round = encode_sample(decode_sample(u));
            if u == 0x7F {
                // The one legitimate alias: 0x7F is negative zero. It decodes to
                // zero, and zero encodes to the positive-zero code, so this is the
                // single byte that does not come back. Asserted rather than skipped,
                // because a second such byte would be a bug.
                assert_eq!(round, MULAW_SILENCE, "negative zero folds onto silence");
            } else {
                assert_eq!(round, u, "code {u:#04x} is not a fixed point");
            }
        }
    }

    /// Values from the standard. These pin the curve itself: an implementation with
    /// the wrong bias or a shifted exponent still round-trips, and still fails here.
    #[test]
    fn the_curve_matches_the_standard() {
        assert_eq!(encode_sample(0), 0xFF);
        assert_eq!(decode_sample(0xFF), 0);
        assert_eq!(decode_sample(0x7F), 0);
        assert_eq!(decode_sample(0x80), 32124);
        assert_eq!(decode_sample(0x00), -32124);
        // Anything past the codec's range comes back as its largest magnitude,
        // rather than wrapping round to the opposite sign.
        assert_eq!(encode_sample(i16::MAX), 0x80);
        assert_eq!(encode_sample(i16::MIN), 0x00);
        assert_eq!(decode_sample(encode_sample(i16::MAX)), 32124);
        assert_eq!(decode_sample(encode_sample(i16::MIN)), -32124);
    }

    /// Louder codes must decode louder, within each sign. An off-by-one in the
    /// exponent leaves the fixed points intact and puts a fold in the curve here.
    #[test]
    fn the_curve_only_ever_rises() {
        // Positive magnitudes run from 0xFF down to 0x80.
        let mut previous = decode_sample(0xFF);
        for u in (0x80..0xFFu8).rev() {
            let v = decode_sample(u);
            assert!(v > previous, "positive code {u:#04x} does not rise: {v} after {previous}");
            previous = v;
        }
        // And negative ones from 0x7F down to 0x00.
        let mut previous = decode_sample(0x7F);
        for u in (0x00..0x7Fu8).rev() {
            let v = decode_sample(u);
            assert!(v < previous, "negative code {u:#04x} does not fall: {v} after {previous}");
            previous = v;
        }
    }

    /// The step of the mu-law quantiser doubles from one segment to the next, so a
    /// single tolerance across the range is meaningless: it is eight samples wide
    /// near silence and over a thousand near full scale. The bound is computed from
    /// the standard's own segment table, independently of the code under test.
    ///
    /// Only the range the codec represents is in scope. Past that it clips, which is
    /// a deliberately larger error and is pinned above.
    #[test]
    fn the_error_stays_inside_the_quantiser_step() {
        let mut x = i16::MIN as i32;
        let mut checked = 0;
        while x <= i16::MAX as i32 {
            let sample = x as i16;
            // The standard's own order: down to 14 bits first, magnitude second. Doing
            // it the other way round puts samples on the far side of a segment
            // boundary and asks for an accuracy the codec never promised there.
            let magnitude = ((sample as i32) >> 2).abs();
            if magnitude > CLIP_14 {
                x += 7;
                continue;
            }
            checked += 1;
            let back = decode_sample(encode_sample(sample)) as i32;
            let biased = magnitude + (BIAS >> 2);
            let seg = segment(biased).min(7);
            // Half a step in the 14-bit domain is `1 << seg`, which is `1 << (seg + 2)`
            // once scaled back up, plus the three counts dropped on the way down.
            let bound = (1i32 << (seg + 2)) + 3;
            assert!(
                (back - x).abs() <= bound,
                "{x} came back as {back}, past the {bound} the standard allows in segment {seg}"
            );
            x += 7; // a stride that is coprime with the segment boundaries
        }
        assert!(checked > 9_000, "only {checked} samples were actually in range");
    }

    /// The property the per-sample bound cannot show: that the curve as a whole is
    /// the right shape. Computed from the signal, with no reference to the codec's
    /// internals, so a mangled segment table cannot pass it.
    #[test]
    fn speech_level_audio_keeps_its_signal_to_noise_ratio() {
        use std::f32::consts::PI;
        // A 440 Hz tone at -6 dBFS, one second of it at the rate of a line.
        let amplitude = i16::MAX as f32 * 10f32.powf(-6.0 / 20.0);
        let input: Vec<i16> = (0..TELEPHONY_RATE)
            .map(|n| {
                let t = n as f32 / TELEPHONY_RATE as f32;
                to_i16(amplitude * (2.0 * PI * 440.0 * t).sin())
            })
            .collect();
        let output = decode(&encode(&input));

        let signal: f64 = input.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let noise: f64 = input
            .iter()
            .zip(&output)
            .map(|(&a, &b)| {
                let d = a as f64 - b as f64;
                d * d
            })
            .sum();
        let snr_db = 10.0 * (signal / noise).log10();
        // The standard puts this near 38 dB across the useful range.
        assert!(snr_db >= 33.0, "signal to noise ratio fell to {snr_db:.1} dB");
    }

    /// A steady level must come out at the same level. This is the test that catches
    /// an unnormalised filter, and the one that catches interpolation without the
    /// gain that inserting zeroes costs: getting that wrong makes the whole call
    /// quiet by half, which nothing on the browser path would ever show.
    #[test]
    fn a_steady_level_keeps_its_level_in_both_directions() {
        for (name, mut r, count) in [
            ("up", Resampler::up_8k_to_16k(), 800),
            ("down", Resampler::down_24k_to_8k(), 2400),
        ] {
            let out = r.process(&vec![1000i16; count]);
            // Skip the filter's delay: the first samples are still ramping up.
            let settled = &out[out.len() / 2..];
            for &s in settled {
                assert!(
                    (s as i32 - 1000).abs() <= 2,
                    "{name} moved a steady 1000 to {s}"
                );
            }
        }
    }

    /// Speech frequencies must pass through untouched.
    #[test]
    fn the_speech_band_passes_through() {
        for hz in [300.0f32, 1000.0] {
            let input = sine(hz, 24_000.0, 4800);
            let out = Resampler::down_24k_to_8k().process(&input);
            let before = rms(&input);
            let after = rms(&out[out.len() / 2..]);
            let ratio = after / before;
            assert!(
                (ratio - 1.0).abs() <= 0.05,
                "{hz} Hz came through at {ratio:.3} of its level"
            );
        }
    }

    /// The test that proves the anti-aliasing is real. 6 kHz cannot exist at 8 kHz:
    /// dropped straight to every third sample it would come back as a 2 kHz tone at
    /// full strength, audible and entirely invented. It has to be removed first.
    #[test]
    fn a_tone_too_high_for_the_line_is_removed_and_not_folded() {
        let input = sine(6000.0, 24_000.0, 4800);
        let out = Resampler::down_24k_to_8k().process(&input);
        let ratio = rms(&out[out.len() / 2..]) / rms(&input);
        assert!(ratio <= 0.02, "a 6 kHz tone survived at {ratio:.4} of its level");
    }

    /// Frame by frame must equal all at once, exactly. A filter that forgot the
    /// previous frame restarts from silence 50 times a second, and every one of
    /// those restarts is a click.
    #[test]
    fn frames_join_without_a_seam() {
        let whole = sine(1000.0, 24_000.0, 4800);
        let at_once = Resampler::down_24k_to_8k().process(&whole);

        let mut framed = Vec::new();
        let mut r = Resampler::down_24k_to_8k();
        for frame in whole.chunks(480) {
            framed.extend(r.process(frame));
        }
        assert_eq!(at_once, framed, "the seams changed the audio");
    }

    /// A whole number of frames in is a whole number of frames out. The pacer counts
    /// on this to know how much audio it is holding.
    #[test]
    fn a_frame_in_is_a_frame_out() {
        assert_eq!(Resampler::down_24k_to_8k().process(&[0i16; 480]).len(), FRAME_SAMPLES);
        assert_eq!(Resampler::up_8k_to_16k().process(&[0i16; 160]).len(), 320);
        // And across many calls, without drift.
        let mut r = Resampler::down_24k_to_8k();
        for _ in 0..10 {
            assert_eq!(r.process(&[0i16; 480]).len(), FRAME_SAMPLES);
        }
    }

    #[test]
    fn nothing_in_is_nothing_out() {
        assert!(Resampler::up_8k_to_16k().process(&[]).is_empty());
        assert!(Framer::new(MULAW_FRAME_BYTES).push(&[]).is_empty());
    }

    #[test]
    fn framing_cuts_whole_frames_and_holds_the_rest() {
        let mut f = Framer::new(MULAW_FRAME_BYTES);
        let frames = f.push(&vec![7u8; 400]);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|fr| fr.len() == MULAW_FRAME_BYTES));
        assert_eq!(f.pending(), 80);
    }

    /// However the bytes arrive, what comes out is the same stream in the same order.
    #[test]
    fn framing_never_alters_the_stream() {
        let source: Vec<u8> = (0..1000u32).map(|n| (n % 251) as u8).collect();
        for step in [1usize, 7, 160, 1000] {
            let mut f = Framer::new(MULAW_FRAME_BYTES);
            let mut seen = Vec::new();
            let mut count = 0;
            for chunk in source.chunks(step) {
                for frame in f.push(chunk) {
                    count += 1;
                    seen.extend(frame);
                }
            }
            assert_eq!(count, source.len() / MULAW_FRAME_BYTES, "step {step}: wrong frame count");
            assert_eq!(seen, source[..seen.len()], "step {step}: the bytes moved");
            assert_eq!(f.pending(), source.len() % MULAW_FRAME_BYTES);
        }
    }

    /// The tail is padded with silence, and silence in mu-law is not zero.
    #[test]
    fn a_short_tail_is_padded_with_silence() {
        let mut f = Framer::new(MULAW_FRAME_BYTES);
        assert!(f.push(&[1, 2, 3]).is_empty());
        let tail = f.flush(MULAW_SILENCE).expect("a tail to flush");
        assert_eq!(tail.len(), MULAW_FRAME_BYTES);
        assert_eq!(&tail[..3], &[1, 2, 3]);
        assert!(tail[3..].iter().all(|&b| b == MULAW_SILENCE), "padded with something audible");
        assert_eq!(decode_sample(MULAW_SILENCE), 0, "and that padding really is silence");
        assert_eq!(f.pending(), 0);
        assert!(f.flush(MULAW_SILENCE).is_none(), "nothing left to flush twice");
    }

    fn sine(hz: f32, rate: f32, count: usize) -> Vec<i16> {
        use std::f32::consts::PI;
        (0..count).map(|n| to_i16(8000.0 * (2.0 * PI * hz * n as f32 / rate).sin())).collect()
    }

    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / samples.len() as f64).sqrt()
    }
}
