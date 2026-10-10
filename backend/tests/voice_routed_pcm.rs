// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use fosnie_backend::voice::{stt_stream::pcm_to_wav,telephony::routed_audio::{PcmReply,SAMPLE_RATE,MAX_SECONDS}};

#[test]
fn wav_layout_is_verified_before_exposing_samples() {
    let pcm=vec![0,1,255,127,0,128];let wav=pcm_to_wav(&pcm,SAMPLE_RATE);
    assert_eq!(PcmReply::from_wav(&wav,"audio/wav; charset=binary").unwrap().samples(),pcm);
    for n in 0..wav.len() {assert!(PcmReply::from_wav(&wav[..n],"audio/wav").is_err(),"truncation {n}");}
    for (offset,value) in [(0,b'X'),(8,b'X'),(20,3),(22,2),(24,1),(28,1),(32,4),(34,8)] {
        let mut bad=wav.clone();bad[offset]=value;assert!(PcmReply::from_wav(&bad,"audio/wav").is_err(),"field {offset}");
    }
    assert!(PcmReply::from_wav(&wav,"audio/mpeg").is_err());
    assert!(PcmReply::from_wav(&pcm_to_wav(&pcm,16000),"audio/wav").is_err());
    assert!(PcmReply::from_wav(&pcm_to_wav(&[],SAMPLE_RATE),"audio/wav").is_err());
    let mut bad=wav.clone();bad[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(PcmReply::from_wav(&bad,"audio/wav").is_err());
}

#[test]
fn padded_metadata_is_allowed_but_duplicate_chunks_and_trailing_bytes_are_not() {
    let wav=pcm_to_wav(&[0,1,2,3],SAMPLE_RATE);
    let mut tagged=wav[..12].to_vec();tagged.extend_from_slice(b"JUNK\x01\x00\x00\x00x\x00");tagged.extend_from_slice(&wav[12..]);
    let size=(tagged.len()-8) as u32;tagged[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(PcmReply::from_wav(&tagged,"application/octet-stream").unwrap().samples(),[0,1,2,3]);
    for extra in [&wav[12..36],&wav[36..],&[0u8][..]] {
        let mut bad=wav.clone();bad.extend_from_slice(extra);let size=(bad.len()-8) as u32;bad[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(PcmReply::from_wav(&bad,"audio/wav").is_err());
    }
    let mut odd=pcm_to_wav(&[1,2,3],SAMPLE_RATE);odd.push(0);let size=(odd.len()-8) as u32;odd[4..8].copy_from_slice(&size.to_le_bytes());
    assert!(PcmReply::from_wav(&odd,"audio/wav").is_err());
}

#[test]
fn bounded_duration_rejects_oversize_audio_even_with_valid_container() {
    let pcm=vec![0u8;SAMPLE_RATE as usize*2*MAX_SECONDS];
    assert!(PcmReply::from_wav(&pcm_to_wav(&pcm,SAMPLE_RATE),"audio/wav").is_ok());
    let mut pcm=pcm;pcm.extend_from_slice(&[0,0]);
    assert!(PcmReply::from_wav(&pcm_to_wav(&pcm,SAMPLE_RATE),"audio/wav").is_err());
}
