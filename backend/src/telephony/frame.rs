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

//! The wire format a telephone system on the practice's own network speaks.
//!
//! Three bytes of header and a payload: one byte saying what this is, then the payload's
//! length as a sixteen-bit **big-endian** number. That is the whole protocol, which is
//! what makes it a good thing to speak to a telephone system nobody here operates.
//!
//! Everything in this module is pure, and the tests below are the substance of it. A
//! stream reassembled wrongly does not fail: it produces a call of loud noise, which
//! nobody discovers until somebody rings up, so the awkward cases are settled here rather
//! than observed there. The awkward cases are all the same shape, and all of them happen:
//! a message split across reads, two messages in one read, a header that arrived without
//! its payload, and a length longer than anything this protocol ever carries.

/// What one message is.
///
/// A closed set of the ones this build acts on. Anything else is skipped by its own length
/// rather than treated as an error, because a telephone system is somebody else's software
/// on somebody else's release schedule: a message type added by a later version must not
/// be able to end a call by existing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The far end has hung up.
    Hangup,
    /// Which call this connection is carrying, as sixteen bytes.
    Id([u8; 16]),
    /// One key pressed, as the character on it.
    Dtmf(char),
    /// Signed sixteen-bit little-endian mono samples.
    Audio(Vec<u8>),
    /// The far end reporting a fault, with whatever it chose to say about it.
    Error(Vec<u8>),
    /// Something this build does not act on, kept only so a reader can say it read it.
    Unknown(u8),
}

/// Message type bytes, as the protocol defines them.
pub const T_HANGUP: u8 = 0x00;
pub const T_ID: u8 = 0x01;
pub const T_DTMF: u8 = 0x03;
/// Audio at eight kilohertz, which is what a telephone line carries. The protocol numbers
/// the higher rates upwards from here; this deployment neither sends nor expects them.
pub const T_AUDIO: u8 = 0x10;
pub const T_ERROR: u8 = 0xff;

/// The longest payload this reader will assemble.
///
/// Far above the 320 bytes of a twenty millisecond frame and far below anything that could
/// exhaust memory. The length field allows sixty-four kilobytes, and something claiming to
/// send that much in one message is not a telephone system: it is a mistake or somebody
/// trying one on, and either way it ends the connection rather than being buffered.
pub const MAX_PAYLOAD: usize = 8_192;

/// One twenty millisecond frame of narrowband audio: 160 samples, two bytes each.
pub const AUDIO_FRAME_BYTES: usize = 320;

/// Put one message on the wire.
pub fn encode(msg: &Message) -> Vec<u8> {
    let (kind, payload): (u8, &[u8]) = match msg {
        Message::Hangup => (T_HANGUP, &[]),
        Message::Id(id) => (T_ID, id.as_slice()),
        Message::Audio(bytes) => (T_AUDIO, bytes.as_slice()),
        Message::Error(bytes) => (T_ERROR, bytes.as_slice()),
        // Neither is ever sent by this end: keys are pressed at the other one, and an
        // unknown message is by definition one this build has nothing to say with.
        Message::Dtmf(_) | Message::Unknown(_) => (T_HANGUP, &[]),
    };
    let mut out = Vec::with_capacity(3 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A message, and the bytes that were not part of it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// One message, and how many bytes of the buffer it used.
    Got(Message, usize),
    /// Nothing whole yet. What is there is a prefix of something longer.
    More,
    /// The far end is not speaking this protocol. The connection ends.
    Broken(&'static str),
}

/// Read the first message out of whatever has arrived so far.
///
/// Returns how many bytes it consumed rather than modifying anything, so the caller owns
/// its buffer and this stays a function of its input.
pub fn step(buf: &[u8]) -> Step {
    if buf.len() < 3 {
        return Step::More;
    }
    let kind = buf[0];
    let len = u16::from_be_bytes([buf[1], buf[2]]) as usize;
    if len > MAX_PAYLOAD {
        // Not assembled, not allocated: refused. Sixty-four kilobytes in one message is
        // not something a telephone line produces.
        return Step::Broken("a message longer than this protocol ever carries");
    }
    if buf.len() < 3 + len {
        return Step::More;
    }
    let payload = &buf[3..3 + len];
    let used = 3 + len;
    let msg = match kind {
        T_HANGUP => Message::Hangup,
        T_ID => match <[u8; 16]>::try_from(payload) {
            Ok(id) => Message::Id(id),
            // A call identifier that is not sixteen bytes identifies no call. Nothing can
            // be done with it, and guessing would mean carrying a call for the wrong one.
            Err(_) => return Step::Broken("a call identifier of the wrong length"),
        },
        T_DTMF => match payload.first() {
            Some(&b) => Message::Dtmf(b as char),
            None => Message::Unknown(kind),
        },
        T_AUDIO => Message::Audio(payload.to_vec()),
        T_ERROR => Message::Error(payload.to_vec()),
        other => Message::Unknown(other),
    };
    Step::Got(msg, used)
}

/// Samples out of an audio payload.
///
/// Little-endian, and an odd trailing byte is dropped rather than joined to the next
/// message: half a sample read from the wrong byte is not a click but static for as long
/// as the misalignment lasts, and a payload is a whole number of samples or it is wrong.
pub fn samples(payload: &[u8]) -> Vec<i16> {
    payload.chunks_exact(2).map(|p| i16::from_le_bytes([p[0], p[1]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) -> Message {
        let bytes = encode(&msg);
        match step(&bytes) {
            Step::Got(back, used) => {
                assert_eq!(used, bytes.len(), "the whole message should have been consumed");
                back
            }
            other => panic!("{msg:?} did not survive the wire: {other:?}"),
        }
    }

    #[test]
    fn every_message_this_end_sends_survives_the_wire() {
        assert_eq!(roundtrip(Message::Hangup), Message::Hangup);
        assert_eq!(roundtrip(Message::Id([7; 16])), Message::Id([7; 16]));
        let audio = vec![1u8; AUDIO_FRAME_BYTES];
        assert_eq!(roundtrip(Message::Audio(audio.clone())), Message::Audio(audio));
        assert_eq!(roundtrip(Message::Error(b"nope".to_vec())), Message::Error(b"nope".to_vec()));
    }

    /// The length is big-endian, which is the one detail of this protocol that is easy to
    /// get backwards and impossible to notice afterwards: a frame read at the wrong length
    /// is static rather than an error.
    #[test]
    fn the_length_is_big_endian() {
        let bytes = encode(&Message::Audio(vec![0; 320]));
        assert_eq!(&bytes[..3], &[T_AUDIO, 0x01, 0x40], "320 is 0x0140, high byte first");
    }

    /// A telephone line delivers whatever the network gives it, which is not the same
    /// thing as one message per read.
    #[test]
    fn a_message_split_across_three_reads_is_read_once_whole() {
        let whole = encode(&Message::Audio(vec![9; 320]));
        let mut buf: Vec<u8> = Vec::new();
        // The header, split in the middle of the length.
        buf.extend_from_slice(&whole[..2]);
        assert_eq!(step(&buf), Step::More);
        buf.extend_from_slice(&whole[2..100]);
        assert_eq!(step(&buf), Step::More);
        buf.extend_from_slice(&whole[100..]);
        match step(&buf) {
            Step::Got(Message::Audio(a), used) => {
                assert_eq!(a.len(), 320);
                assert_eq!(used, whole.len());
            }
            other => panic!("the reassembled message read as {other:?}"),
        }
    }

    #[test]
    fn two_messages_in_one_read_are_read_in_turn() {
        let mut buf = encode(&Message::Id([3; 16]));
        buf.extend_from_slice(&encode(&Message::Audio(vec![1; 4])));
        let Step::Got(first, used) = step(&buf) else { panic!("no first message") };
        assert_eq!(first, Message::Id([3; 16]));
        match step(&buf[used..]) {
            Step::Got(Message::Audio(a), _) => assert_eq!(a, vec![1; 4]),
            other => panic!("the second message read as {other:?}"),
        }
    }

    /// A header with nothing behind it yet is a prefix, not a message with an empty
    /// payload. Reading it as the latter would hang up on a caller mid-sentence.
    #[test]
    fn a_header_without_its_payload_waits() {
        let whole = encode(&Message::Audio(vec![5; 320]));
        assert_eq!(step(&whole[..3]), Step::More);
        assert_eq!(step(&whole[..3 + 319]), Step::More);
        assert!(matches!(step(&whole), Step::Got(_, _)));
    }

    /// An empty payload IS a whole message for the types that have none.
    #[test]
    fn a_message_with_no_payload_is_still_a_message() {
        assert_eq!(step(&[T_HANGUP, 0, 0]), Step::Got(Message::Hangup, 3));
    }

    #[test]
    fn a_length_beyond_anything_this_protocol_carries_is_refused_not_buffered() {
        // Claims sixty-four kilobytes, sends three bytes. Nothing is allocated for it.
        let claim = [T_AUDIO, 0xff, 0xff];
        assert!(matches!(step(&claim), Step::Broken(_)));
        // And the boundary is where it says it is.
        let ok = [T_AUDIO, (MAX_PAYLOAD >> 8) as u8, (MAX_PAYLOAD & 0xff) as u8];
        assert_eq!(step(&ok), Step::More, "a payload at the cap is waited for, not refused");
    }

    /// Somebody else's software on somebody else's release schedule. A type this build
    /// has never heard of is stepped over by its own length, not treated as a fault.
    #[test]
    fn a_message_this_build_does_not_know_is_stepped_over() {
        let mut buf = vec![0x42, 0x00, 0x04, 1, 2, 3, 4];
        let after = encode(&Message::Hangup);
        buf.extend_from_slice(&after);
        match step(&buf) {
            Step::Got(Message::Unknown(0x42), used) => {
                assert_eq!(used, 7);
                assert_eq!(step(&buf[used..]), Step::Got(Message::Hangup, 3));
            }
            other => panic!("an unknown message read as {other:?}"),
        }
    }

    /// A call identifier is sixteen bytes. One that is not identifies nothing, and a
    /// connection that cannot say which call it is carrying has nothing to carry.
    #[test]
    fn a_call_identifier_of_the_wrong_length_ends_the_connection() {
        assert!(matches!(step(&[T_ID, 0x00, 0x04, 1, 2, 3, 4]), Step::Broken(_)));
    }

    #[test]
    fn a_pressed_key_is_read_as_the_character_on_it() {
        assert_eq!(step(&[T_DTMF, 0x00, 0x01, b'5']), Step::Got(Message::Dtmf('5'), 4));
        assert_eq!(step(&[T_DTMF, 0x00, 0x01, b'#']), Step::Got(Message::Dtmf('#'), 4));
    }

    #[test]
    fn samples_are_little_endian_and_whole() {
        assert_eq!(samples(&[0x00, 0x01, 0xff, 0xff]), vec![256, -1]);
        // A stray byte is dropped rather than joined to whatever comes next.
        assert_eq!(samples(&[0x00, 0x01, 0x7f]), vec![256]);
        assert_eq!(samples(&[]), Vec::<i16>::new());
    }

    /// The frame this end sends is twenty milliseconds of narrowband audio, and the
    /// arithmetic behind that number is stated rather than assumed.
    #[test]
    fn a_frame_is_twenty_milliseconds_of_narrowband_audio() {
        assert_eq!(AUDIO_FRAME_BYTES, 320);
        assert_eq!(AUDIO_FRAME_BYTES, 8_000 * 20 / 1000 * 2);
    }
}
