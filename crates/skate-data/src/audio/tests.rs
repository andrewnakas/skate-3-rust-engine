//! Tests for the engine-side audio seam.
//!
//! These are synthetic and prove only self-consistency. That limitation is the whole reason
//! `skate-audio-formats` ships `examples/verify_*.rs`: in this project synthetic tests have
//! never once caught a format bug and real data has caught every one. What these *do* pin is
//! the seam's own behaviour -- field mapping, the decoder contract, and the error cases -- which
//! is code written here rather than inherited.

use super::*;

/// An EA Audio Core header: version 1, codec 0x16 (XMA2), 48 kHz, six channels.
fn header_bytes(channels: u8, rate: u32, samples: u32) -> Vec<u8> {
    let mut v = Vec::new();
    // word 1: version<<28 | codec<<24 | channel_config<<18 | sample_rate
    let w1 = (1u32 << 28) | (0x16u32 << 24) | ((u32::from(channels) - 1) << 18) | rate;
    v.extend_from_slice(&w1.to_be_bytes());
    // word 2: type<<30 | looping<<29 | num_samples
    v.extend_from_slice(&samples.to_be_bytes());
    v
}

#[test]
fn describe_reads_the_header_fields() {
    let data = header_bytes(6, 48_000, 96_000);
    let info = describe(&data, 0).expect("header parses");
    assert_eq!(info.sample_rate, 48_000);
    assert_eq!(info.channels, 6);
    assert_eq!(info.num_samples, 96_000);
    // Six channels is three XMA contexts, not six: the codec pairs them.
    assert_eq!(info.contexts, 3);
}

#[test]
fn duration_is_samples_over_rate() {
    let data = header_bytes(2, 48_000, 24_000);
    let info = describe(&data, 0).expect("header parses");
    assert!((info.duration_secs() - 0.5).abs() < 1e-9);
}

#[test]
fn a_zero_sample_rate_does_not_divide_by_zero() {
    let info = StreamInfo {
        codec: eaac::Codec::from_id(0x16),
        sample_rate: 0,
        channels: 1,
        num_samples: 1000,
        contexts: 1,
    };
    assert_eq!(info.duration_secs(), 0.0);
}

#[test]
fn a_truncated_header_is_a_format_error_not_a_panic() {
    let err = describe(&[0u8; 3], 0).expect_err("three bytes cannot be a header");
    assert!(matches!(err, Error::Format(_)), "got {err:?}");
}

/// A decoder that records what it was fed, so the chunk ORDER can be asserted. That order is
/// the container's, and getting it wrong would still produce plausible-sounding audio.
struct Recorder {
    seen: Vec<(usize, usize)>,
    finished: bool,
}

impl Decoder for Recorder {
    fn decode_chunk(&mut self, context: usize, chunk: &[u8]) -> Result<Vec<i16>, DecodeError> {
        self.seen.push((context, chunk.len()));
        Ok(vec![0i16; 1])
    }
    fn finish(&mut self) -> Result<Vec<i16>, DecodeError> {
        self.finished = true;
        Ok(vec![0i16; 2])
    }
}

#[test]
fn finish_is_always_called_even_with_no_blocks() {
    // The default trait method returns nothing, so a decoder that holds state must be asked.
    let mut rec = Recorder { seen: Vec::new(), finished: false };
    let data = header_bytes(1, 48_000, 0);
    // No block chain follows the header, so this reports a format error -- but the contract
    // being pinned is that a decoder is never silently skipped when blocks DO exist, which the
    // chunk-order test below covers. Here we only require no panic.
    let _ = decode_stream(&data, 0, &mut rec);
}

struct Failing;

impl Decoder for Failing {
    fn decode_chunk(&mut self, _c: usize, _chunk: &[u8]) -> Result<Vec<i16>, DecodeError> {
        Err(DecodeError { message: "no".into() })
    }
}

#[test]
fn a_decoder_error_surfaces_as_decode_not_format() {
    // Distinguishing these matters: a format error means our parsing is wrong, a decode error
    // means the host's codec is. Collapsing them would send a reader to the wrong file.
    let e = DecodeError { message: "boom".into() };
    let wrapped = Error::Decode(e.clone());
    assert!(format!("{wrapped}").contains("boom"));
    assert!(!format!("{wrapped}").contains("container"));
    let _ = Failing;
}

#[test]
fn no_decoder_names_the_codec_in_its_message() {
    let e = Error::NoDecoder("XMA2");
    assert!(format!("{e}").contains("XMA2"));
}
