//! Skate 3 audio graph, ported from the shadow-verified native C++.
//!
//! Source: `skate3recomp-dev/src/skate3_audio_native.cpp`, each function of which was
//! checked against the original recompiled body under the shadow harness before being
//! translated here. Offsets come from `docs/rw_audio_structs.h` and are asserted below the
//! way `docs/rw_audio_structs_check.c` asserts them in C.
//!
//! Everything here operates on **guest memory**: big-endian, byte-addressed, at guest
//! addresses. That is deliberate rather than idiomatic. These are recovered layouts with
//! asserted offsets, and Phase 4's per-function criterion compares *bytes* against the
//! verified C++ — so a byte-addressed view makes that comparison direct, instead of routing
//! it through a serialisation step that could hide a discrepancy of its own.
//!
//! Unsafe is not forbidden crate-wide the way it is in `skate-audio-formats`, because the
//! DSP modules will scope `unsafe` to intrinsic calls. Nothing in this file needs it.

pub mod buffers;
pub mod player;
pub mod system;

/// One contiguous span of guest memory.
#[derive(Clone, Debug)]
pub struct Segment {
    pub base: u32,
    pub bytes: Vec<u8>,
}

/// Guest memory as a set of spans.
///
/// Segmented rather than one flat slice, and that correction came from real data: the first
/// recorded vector put a buffer-pair object at `0x7018E110` on the guest stack with its buffers
/// at `0x401736D0` on the heap — about 768 MB apart, which no single slice can span. The flat
/// model was an assumption, and eleven synthetic tests passed against it happily because they
/// used one tidy contiguous window.
#[derive(Clone, Debug, Default)]
pub struct Guest {
    segments: Vec<Segment>,
}

/// Out-of-window access, reported rather than panicking silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub address: u32,
    pub message: String,
}

impl Error {
    pub fn new(address: u32, message: impl Into<String>) -> Self {
        Self { address, message: message.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at {:#010x}: {}", self.address, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

impl Guest {
    /// A single span, which is what the unit tests want.
    pub fn single(base: u32, len: usize) -> Self {
        Self { segments: vec![Segment { base, bytes: vec![0u8; len] }] }
    }

    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self { segments }
    }

    /// Add a span, or overwrite an existing one with the same base.
    pub fn put(&mut self, base: u32, bytes: Vec<u8>) {
        match self.segments.iter_mut().find(|s| s.base == base) {
            Some(s) => s.bytes = bytes,
            None => self.segments.push(Segment { base, bytes }),
        }
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    fn locate(&self, ea: u32, len: usize) -> Result<(usize, usize)> {
        for (i, s) in self.segments.iter().enumerate() {
            if ea >= s.base {
                let off = (ea - s.base) as usize;
                if off + len <= s.bytes.len() {
                    return Ok((i, off));
                }
            }
        }
        Err(Error::new(ea, "no segment covers this address"))
    }

    pub fn u32(&self, ea: u32) -> Result<u32> {
        let (i, o) = self.locate(ea, 4)?;
        let b = &self.segments[i].bytes;
        Ok(u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
    }

    pub fn set_u32(&mut self, ea: u32, value: u32) -> Result<()> {
        let (i, o) = self.locate(ea, 4)?;
        self.segments[i].bytes[o..o + 4].copy_from_slice(&value.to_be_bytes());
        Ok(())
    }

    pub fn u8(&self, ea: u32) -> Result<u8> {
        let (i, o) = self.locate(ea, 1)?;
        Ok(self.segments[i].bytes[o])
    }

    pub fn set_u8(&mut self, ea: u32, value: u8) -> Result<()> {
        let (i, o) = self.locate(ea, 1)?;
        self.segments[i].bytes[o] = value;
        Ok(())
    }

    /// `f32` from the guest's big-endian bits, without touching the value.
    pub fn f32(&self, ea: u32) -> Result<f32> {
        Ok(f32::from_bits(self.u32(ea)?))
    }

    pub fn fill(&mut self, ea: u32, value: u8, len: u32) -> Result<()> {
        let (i, o) = self.locate(ea, len as usize)?;
        self.segments[i].bytes[o..o + len as usize].fill(value);
        Ok(())
    }

    /// The bytes of the span starting exactly at `base`, for comparing against a recorded
    /// expectation.
    pub fn span(&self, base: u32, len: usize) -> Result<&[u8]> {
        let (i, o) = self.locate(base, len)?;
        Ok(&self.segments[i].bytes[o..o + len])
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::Guest;

    /// Guest addresses used by the tests. The window is flat, so everything lives inside it.
    pub const BASE: u32 = 0x4000_0000;
    pub const SYSTEM: u32 = 0x4000_0000;
    pub const RING: u32 = 0x4000_0200;
    pub const PLAYER: u32 = 0x4000_0400;
    pub const PACKET_A: u32 = 0x4000_0600;
    pub const PACKET_B: u32 = 0x4000_0620;
    pub const PARAMS: u32 = 0x4000_0700;
    pub const SOURCE: u32 = 0x4000_0740;

    pub fn guest() -> Guest {
        let mut g = Guest::single(BASE, 0x800);
        wire(&mut g);
        g
    }

    /// A player wired to a system with an empty ring, and a source object.
    pub fn wire(g: &mut Guest) {
        g.set_u32(PLAYER + crate::system::PLAYER_SYSTEM, SYSTEM).unwrap();
        g.set_u32(SYSTEM + crate::system::SYSTEM_CMD_BUFFER, RING).unwrap();
        g.set_u32(SYSTEM + crate::system::SYSTEM_CMD_WRITE_OFF, 0).unwrap();
        g.set_u32(PLAYER + crate::player::PLAYER_SOURCE, SOURCE).unwrap();
    }
}
