//! EA Audio Core stream headers and block chains.
//!
//! Every waveform on the Skate 3 disc is EAAC with codec id 3 (XMA); no RIFF wrapper
//! and no `XMA2` chunk exists anywhere, because EA re-blocks the XMA bitstream and
//! strips packet padding. That is why the runtime feeds hardware XMA contexts directly
//! rather than handing off a file.
//!
//! Two big-endian u32s precede the payload:
//!
//! ```text
//! header1  [4b version][4b codec][6b channel_config][18b sample_rate]
//! header2  [2b stream_type][1b loop][29b num_samples]
//! ```

use crate::{be32, Error, Result};

/// Codec identifier from the stream header. Only [`Codec::Xma`] occurs on the disc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Xma,
    Other(u8),
}

impl Codec {
    pub fn from_id(id: u8) -> Self {
        match id {
            3 => Self::Xma,
            v => Self::Other(v),
        }
    }
}

/// A decoded EAAC stream header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u8,
    pub codec: Codec,
    /// Raw field; [`Header::channels`] is the usable count.
    pub channel_config: u8,
    pub sample_rate: u32,
    pub stream_type: u8,
    pub looping: bool,
    pub num_samples: u32,
}

impl Header {
    pub const SIZE: usize = 8;

    pub fn parse(data: &[u8], at: usize) -> Result<Self> {
        let h1 = be32(data, at)?;
        let h2 = be32(data, at + 4)?;
        let header = Self {
            version: (h1 >> 28) as u8 & 0xF,
            codec: Codec::from_id((h1 >> 24) as u8 & 0xF),
            channel_config: (h1 >> 18) as u8 & 0x3F,
            sample_rate: h1 & 0x3_FFFF,
            stream_type: (h2 >> 30) as u8 & 0x3,
            looping: (h2 >> 29) & 1 != 0,
            num_samples: h2 & 0x1FFF_FFFF,
        };
        header.check_plausible(at)?;
        Ok(header)
    }

    /// Reject headers that cannot describe real audio.
    ///
    /// Not every file carries a header at offset 0: `.sns` block streams and `.sek`
    /// seek tables keep theirs elsewhere (`.snr` sidecars, or a metadata table in a
    /// companion archive). Without this check those parse "successfully" into
    /// nonsense like 5 channels at 384 Hz, which is worse than an error because it
    /// looks like data. Bounds observed across the whole disc: codec is always 3,
    /// rates are 36000/44100/48000, and channel counts are 1, 2 or 6.
    fn check_plausible(&self, at: usize) -> Result<()> {
        if !matches!(self.sample_rate, 8_000..=48_000) {
            return Err(Error::new(at, format!("implausible sample rate {}", self.sample_rate)));
        }
        if self.channels() > 8 {
            return Err(Error::new(at, format!("implausible channel count {}", self.channels())));
        }
        if self.num_samples == 0 {
            return Err(Error::new(at, "zero samples"));
        }
        Ok(())
    }

    /// Channel count. The header stores one less than the true count.
    pub fn channels(&self) -> u8 {
        self.channel_config + 1
    }

    /// Duration in seconds.
    pub fn duration_secs(&self) -> f64 {
        f64::from(self.num_samples) / f64::from(self.sample_rate)
    }
}

/// One block of a block-chained payload (`.sns`, `.dat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub flags: u8,
    /// Size in bytes **including** this 8-byte header.
    pub size: u32,
    pub num_samples: u32,
    /// Offset of the block header within the payload.
    pub offset: usize,
}

impl Block {
    pub const HEADER_SIZE: usize = 8;

    /// Parse the single block whose header starts at `at` within `data`.
    ///
    /// `data` must be the containing buffer, not a slice trimmed to the header: the
    /// block's declared size is validated against it. Callers walking one sub-sound out
    /// of a shared payload should pass the whole payload and advance `at`.
    pub fn parse(data: &[u8], at: usize) -> Result<Self> {
        let word = be32(data, at)?;
        let size = word & 0x00FF_FFFF;
        if (size as usize) <= Self::HEADER_SIZE {
            return Err(Error::new(at, format!("block size {size} does not advance")));
        }
        if at + size as usize > data.len() {
            return Err(Error::new(at, format!("block size {size} runs past end")));
        }
        Ok(Self {
            flags: (word >> 24) as u8,
            size,
            num_samples: be32(data, at + 4)?,
            offset: at,
        })
    }

    /// Payload byte range of this block, excluding its header.
    pub fn data_range(&self) -> std::ops::Range<usize> {
        let start = self.offset + Self::HEADER_SIZE;
        start..self.offset + self.size as usize
    }
}

/// Walk the block chain of an EAAC payload.
///
/// Stops at the end of the buffer. A block claiming a size that would not advance, or
/// that runs past the end, is an error rather than a silent truncation.
pub fn blocks(data: &[u8]) -> Result<Vec<Block>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + Block::HEADER_SIZE <= data.len() {
        let block = Block::parse(data, at)?;
        at += block.size as usize;
        out.push(block);
    }
    Ok(out)
}

/// One context's slice of a block: the bytes feeding a single hardware XMA context.
///
/// A hardware XMA context decodes at most a stereo pair, so a multichannel stream is
/// split across `ceil(channels / 2)` of them and every block carries one chunk per
/// context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// The raw length field preceding the chunk.
    pub field: u32,
    pub data: &'a [u8],
}

/// Bias subtracted from a chunk's length field before scaling.
pub const LENGTH_BIAS: u32 = 18;

/// Decode a chunk length from its field: `length = (field - 18) >> 2`.
///
/// The low two bits are **not** part of the length. They are constant within a stream
/// and differ between streams: ambience payloads always leave a remainder of 1, speech
/// and music always 0. Measured over 2000 ambience blocks and every speech and music
/// block tested, with no other value seen.
///
/// This was first derived per-container as two different biases, 19 and 18, which fit
/// each class exactly and looked like separate encodings. The parity of the raw fields
/// gave it away -- ambience's are uniformly odd, the others uniformly even -- so it is
/// one encoding with a flag, not two.
///
/// Earlier still these fields were read as **bit offsets**, which they superficially
/// resemble: the values land in the plausible range for an offset into the chunk that
/// follows. A bit offset cannot be a pure function of chunk length, and this is one.
pub fn chunk_length(field: u32) -> Option<u32> {
    let n = field.checked_sub(LENGTH_BIAS)?;
    let len = n >> 2;
    if len == 0 { None } else { Some(len) }
}

/// Split a block payload into one chunk per XMA context.
///
/// Deterministic: each chunk is preceded by its length field, so no scanning or
/// disambiguation is needed.
///
/// The chunks must account for the whole payload **except** for trailing padding: the
/// final block of a stream is padded to an alignment boundary and leaves a few bytes
/// spare (19 and 26 bytes observed; music segments show 7 to 54). Every other block
/// accounts exactly, so a shortfall larger than a small pad still fails.
pub fn split_block(payload: &[u8], contexts: usize) -> Result<Vec<Chunk<'_>>> {
    let mut out = Vec::with_capacity(contexts);
    let mut at = 0usize;
    for i in 0..contexts {
        let field = be32(payload, at)?;
        let len = chunk_length(field).ok_or_else(|| {
            Error::new(at, format!("chunk {i}: field {field} is not a valid length"))
        })? as usize;
        let start = at + 4;
        let end = start.checked_add(len).filter(|e| *e <= payload.len()).ok_or_else(|| {
            Error::new(at, format!("chunk {i}: length {len} runs past the payload"))
        })?;
        out.push(Chunk { field, data: &payload[start..end] });
        at = end;
    }
    // Allow trailing alignment padding on a stream's final block, but nothing larger.
    const MAX_PAD: usize = 64;
    let slack = payload.len() - at;
    if slack > MAX_PAD {
        return Err(Error::new(
            at,
            format!("chunks accounted for {at} of {} payload bytes", payload.len()),
        ));
    }
    Ok(out)
}

/// Number of hardware XMA contexts a channel count needs.
pub fn context_count(channels: u8) -> usize {
    ((channels as usize) + 1) / 2
}

/// A `.snr` sidecar record: the stream header for a `.sns` payload of the same stem.
///
/// Bulk audio is split across two archive members -- `<stem>.snr` carries the header and
/// `<stem>.sns` the block chain -- and they are paired **by name**, not by index or hash.
/// Verified against `ambienceresident.big` / `ambience.big`: 24 stems each, all 24 paired,
/// none left over.
///
/// The record is **variable length**: either the 8-byte header alone, or the header plus
/// 8 further bytes. In `ambienceresident.big` every looping stream carries the extended
/// form and the one non-looping stream (`23_Press_start_screen`) carries the short form,
/// so the trailing bytes are most likely loop metadata. They were zero in every entry
/// sampled, so their meaning is not established and they are exposed raw rather than
/// interpreted.
///
/// An earlier version of this type required 16 bytes, having generalised from three
/// sampled entries that all happened to be extended. That silently dropped the short
/// record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnrRecord {
    pub header: Header,
    /// The 8 trailing bytes, when present.
    pub extended: Option<[u8; 8]>,
}

impl SnrRecord {
    /// Smallest valid record: the header alone.
    pub const MIN_SIZE: usize = Header::SIZE;
    /// Record size when the trailing bytes are present.
    pub const EXTENDED_SIZE: usize = 16;

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::MIN_SIZE {
            return Err(Error::new(
                0,
                format!("`.snr` record is {} bytes, want at least {}", data.len(), Self::MIN_SIZE),
            ));
        }
        let extended = data
            .get(Header::SIZE..Self::EXTENDED_SIZE)
            .map(|b| [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        Ok(Self { header: Header::parse(data, 0)?, extended })
    }
}

/// One sub-sound within a `.sth` record: a header plus where its payload starts.
///
/// Speech is packed differently from ambience. A line of dialogue is a single `.dat`
/// payload holding several sub-sounds back to back, and the matching `.sth` member is an
/// array of 12-byte records -- one per sub-sound -- giving each one's byte offset into
/// that payload alongside its own header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubSound {
    /// Byte offset of this sub-sound's block chain within the `.dat` payload.
    pub data_offset: u32,
    pub header: Header,
}

impl SubSound {
    pub const SIZE: usize = 12;
}

/// Parse a `.sth` member: an array of [`SubSound`] records, one per sub-sound.
///
/// Verified against `announcersth.big`: 405 members, sub-sound counts varying from a few
/// to a few dozen, every one mono 36 kHz codec 3.
pub fn sub_sounds(data: &[u8]) -> Result<Vec<SubSound>> {
    if data.len() % SubSound::SIZE != 0 {
        return Err(Error::new(
            0,
            format!("`.sth` member is {} bytes, not a multiple of 12", data.len()),
        ));
    }
    let mut out = Vec::with_capacity(data.len() / SubSound::SIZE);
    for at in (0..data.len()).step_by(SubSound::SIZE) {
        out.push(SubSound {
            data_offset: be32(data, at)?,
            header: Header::parse(data, at + 4)?,
        });
    }
    Ok(out)
}

/// Strip a known audio extension, giving the stem used to pair `.snr` with `.sns`.
pub fn stem(name: &str) -> Option<&str> {
    for ext in [".snr", ".sns", ".dat", ".sek", ".sth", ".hdr"] {
        if let Some(s) = name.strip_suffix(ext) {
            return Some(s);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_mono_48k_sfx_header() {
        // 0x0300BB80 as observed on wheels.big entries.
        let raw = [0x03, 0x00, 0xBB, 0x80, 0x00, 0x0A, 0xD9, 0x00];
        let h = Header::parse(&raw, 0).unwrap();
        assert_eq!(h.codec, Codec::Xma);
        assert_eq!(h.version, 0);
        assert_eq!(h.channel_config, 0);
        assert_eq!(h.channels(), 1);
        assert_eq!(h.sample_rate, 48_000);
    }

    #[test]
    fn parses_the_stereo_44k1_music_header() {
        // 0x0304AC44 as observed in every .mus SNR table entry.
        let raw = [0x03, 0x04, 0xAC, 0x44, 0x00, 0x00, 0xEF, 0x6C];
        let h = Header::parse(&raw, 0).unwrap();
        assert_eq!(h.codec, Codec::Xma);
        assert_eq!(h.channel_config, 1);
        assert_eq!(h.channels(), 2);
        assert_eq!(h.sample_rate, 44_100);
    }

    #[test]
    fn parses_the_mono_36k_speech_header() {
        // header1 0x03008CA0 as observed in announcerspeech .sth records; header2
        // carries a real sample count (~226k, one line of dialogue at 36 kHz).
        let raw = [0x03, 0x00, 0x8C, 0xA0, 0x00, 0x03, 0x73, 0x10];
        let h = Header::parse(&raw, 0).unwrap();
        assert_eq!(h.sample_rate, 36_000);
        assert_eq!(h.channels(), 1);
        assert!((h.duration_secs() - 6.2).abs() < 0.1);
    }

    #[test]
    fn rejects_a_headerless_sns_payload() {
        // First bytes of a real .sns block stream, which carries no header at 0.
        // Previously this parsed as "1ch 5735Hz" instead of failing.
        let raw = [0x00, 0x00, 0x16, 0x67, 0x00, 0x00, 0x12, 0x80];
        assert!(Header::parse(&raw, 0).is_err());
    }

    #[test]
    fn rejects_a_truncated_header() {
        assert!(Header::parse(&[0x03, 0x00], 0).is_err());
    }

    #[test]
    fn rejects_a_non_advancing_block() {
        let raw = [0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x10];
        assert!(blocks(&raw).is_err());
    }

    #[test]
    fn rejects_a_block_running_past_the_end() {
        let raw = [0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x10];
        assert!(blocks(&raw).is_err());
    }

    #[test]
    fn parses_a_real_ambience_snr_record() {
        // ambienceresident.big / 08_univ_mt_low.snr, verbatim.
        let raw = [
            0x03, 0x10, 0xBB, 0x80, 0x60, 0x6C, 0x13, 0xFF,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let r = SnrRecord::parse(&raw).unwrap();
        assert_eq!(r.header.codec, Codec::Xma);
        assert_eq!(r.header.sample_rate, 48_000);
        assert_eq!(r.header.channel_config, 4);
        assert!(r.header.looping);
        assert_eq!(r.header.num_samples, 7_083_007);
        assert!((r.header.duration_secs() - 147.56).abs() < 0.01);
        assert_eq!(r.extended, Some([0u8; 8]));
    }

    #[test]
    fn accepts_the_short_eight_byte_snr_record() {
        // ambienceresident.big / 23_Press_start_screen.snr is 8 bytes, not 16, and is
        // the only non-looping entry there. Requiring 16 dropped it silently.
        let raw = [0x03, 0x10, 0xBB, 0x80, 0x40, 0x17, 0x0E, 0x7C];
        let r = SnrRecord::parse(&raw).unwrap();
        assert_eq!(r.header.num_samples, 1_511_036);
        assert!(!r.header.looping);
        assert_eq!(r.extended, None);
    }

    #[test]
    fn rejects_a_record_shorter_than_the_header() {
        assert!(SnrRecord::parse(&[0x03, 0x10, 0xBB, 0x80]).is_err());
    }

    #[test]
    fn pairs_by_stem() {
        assert_eq!(stem("08_univ_mt_low.snr"), Some("08_univ_mt_low"));
        assert_eq!(stem("08_univ_mt_low.sns"), Some("08_univ_mt_low"));
        assert_eq!(stem("wbts.txt"), None);
    }

    #[test]
    fn parses_a_real_sth_member() {
        // announcersth.big / 489_35_BantLeadPro_Ratt.sth, first two of five records.
        let raw = [
            0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x8C, 0xA0, 0x40, 0x03, 0x05, 0x6B,
            0x00, 0x00, 0x8C, 0x80, 0x03, 0x00, 0x8C, 0xA0, 0x40, 0x02, 0xD8, 0xF1,
        ];
        let subs = sub_sounds(&raw).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].data_offset, 0);
        assert_eq!(subs[0].header.sample_rate, 36_000);
        assert_eq!(subs[0].header.channels(), 1);
        assert_eq!(subs[0].header.num_samples, 197_995);
        assert_eq!(subs[1].data_offset, 0x8C80);
        assert_eq!(subs[1].header.num_samples, 186_609);
    }

    #[test]
    fn rejects_a_sth_member_of_odd_length() {
        assert!(sub_sounds(&[0u8; 13]).is_err());
    }

    #[test]
    fn decodes_chunk_lengths_from_real_fields() {
        // ambience.big / 08_univ_mt_low.sns block 1, measured lengths 2337/2431/1180.
        assert_eq!(chunk_length(9367), Some(2337));
        assert_eq!(chunk_length(9743), Some(2431));
        assert_eq!(chunk_length(4739), Some(1180));
        // Game_Stream.mus block 0 body, and a speech block -- same formula, and their
        // fields are even where ambience's are odd.
        assert_eq!(chunk_length(8142), Some(2031));
        assert_eq!(chunk_length(8106), Some(2022));
    }

    #[test]
    fn rejects_a_field_that_is_not_a_length() {
        assert_eq!(chunk_length(18), None); // zero length
        assert_eq!(chunk_length(21), None); // still zero after the shift
        assert_eq!(chunk_length(3), None);  // underflows the bias
    }

    #[test]
    fn splits_a_three_context_block() {
        let lens = [12usize, 8, 4];
        let mut payload = Vec::new();
        for l in lens {
            payload.extend_from_slice(&(4 * l as u32 + LENGTH_BIAS).to_be_bytes());
            payload.extend(std::iter::repeat(0xAB).take(l));
        }
        let chunks = split_block(&payload, 3).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.iter().map(|c| c.data.len()).collect::<Vec<_>>(), lens);
    }

    #[test]
    fn rejects_a_split_that_does_not_account_for_the_payload() {
        let mut payload = (4u32 * 8 + LENGTH_BIAS).to_be_bytes().to_vec();
        payload.extend(std::iter::repeat(0).take(8));
        payload.extend(std::iter::repeat(0).take(200)); // far more than alignment padding
        assert!(split_block(&payload, 1).is_err());
    }

    #[test]
    fn context_count_matches_the_hardware_split() {
        assert_eq!(context_count(1), 1); // speech: mono
        assert_eq!(context_count(2), 1); // music: one stereo pair
        assert_eq!(context_count(5), 3); // ambience: 2 + 2 + 1
        assert_eq!(context_count(6), 3);
    }

    #[test]
    fn walks_a_two_block_chain() {
        let mut raw = vec![0u8; 32];
        raw[0..4].copy_from_slice(&0x0000_0010u32.to_be_bytes());
        raw[4..8].copy_from_slice(&1280u32.to_be_bytes());
        raw[16..20].copy_from_slice(&0x0000_0010u32.to_be_bytes());
        raw[20..24].copy_from_slice(&1280u32.to_be_bytes());
        let b = blocks(&raw).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].num_samples, 1280);
        assert_eq!(b[0].data_range(), 8..16);
        assert_eq!(b[1].offset, 16);
    }
}
