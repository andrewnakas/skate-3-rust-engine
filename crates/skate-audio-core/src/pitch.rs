//! `sub_82B2DAC8` — recompute a voice's pitch ratio and advance its fractional position.
//!
//! Ported from `recomp/src/audio_ports/sub_82B2DAC8.inc`, **STATUS: verified** — 306,361 calls in a
//! played session on `RwAudioCore Dac`.
//!
//! Each block, the voice's playback ratio is `requested / source_rate * scale`, two single-rounded
//! operations. When it moved since the last block it is turned into a 16.16 step — scaled, rounded
//! **half away from zero**, truncated — and clamped at 2^18, in which case the stored ratio becomes a
//! pool constant instead. Then, on every call, the step advances a 16.16 position by the block's frame
//! count, the stream's gain is scaled by the ratio, and the function returns how many whole source
//! frames the block consumes, adjusted by two bytes on the voice and clamped at zero.
//!
//! Three details the tests pin, because each is where a plausible rewrite drifts:
//!
//! - **An unchanged ratio skips the whole recompute, stores included.** The comparison is against the
//!   value stored last time, so the +56/+60/+68 words are not rewritten.
//! - **The tail re-reads the step and the ratio from memory**, not from registers.
//! - **The result is 64-bit and the clamp looks only at the low word.** `whole - bias + credit` can
//!   borrow into the upper half; a set sign bit in the low word clamps the whole result to zero, and
//!   a clear one returns all 64 bits.
//!
//! Nothing in `docs/rw_audio_structs.h` names these offsets.

use crate::vmx::Fpscr;
use crate::{fp, Guest, Result};

/// `lis -32245 ; lfs 16660` — the ratio-to-step scale.
pub const RATIO_SCALE: u32 = (((-32245i32 as u32) & 0xFFFF) << 16) + 16660;
/// `lis -32246 ; lfs -26788` — 0.5, the rounding half.
pub const ROUND_HALF: u32 = (((-32246i32 as u32) & 0xFFFF) << 16).wrapping_sub(26788);
/// `lis -32219 ; lfs 29448` — the ratio stored when the step saturates.
pub const RATIO_CEILING: u32 = (((-32219i32 as u32) & 0xFFFF) << 16) + 29448;
/// `lis r10,4` — the most a step can be: 2^18, four frames per sample in 16.16.
pub const STEP_LIMIT: i32 = 262_144;
const _: () = assert!(RATIO_SCALE == 0x820B_4114 && ROUND_HALF == 0x8209_975C);
const _: () = assert!(RATIO_CEILING == 0x8225_7308 && STEP_LIMIT == 4 << 16);

/// The voice's `f32` scale on the requested rate.
pub const VOICE_SCALE: u32 = 52;
/// The ratio in use — the pool ceiling when the step saturated.
pub const VOICE_RATIO: u32 = 56;
/// The ratio computed last time, which decides whether to recompute.
pub const VOICE_PREVIOUS: u32 = 60;
/// The requested rate.
pub const VOICE_REQUESTED: u32 = 64;
/// The 16.16 step.
pub const VOICE_STEP: u32 = 68;
/// The 16.16 fractional position.
pub const VOICE_FRACTION: u32 = 72;
/// `sth r6,78(r3)` — the block's frame count, stored on every call.
pub const VOICE_FRAMES: u32 = 78;
/// A byte subtracted from the whole-frame count.
pub const VOICE_BIAS: u32 = 80;
/// A byte added back to it.
pub const VOICE_CREDIT: u32 = 81;
/// The stream's gain, scaled by the ratio on every call.
pub const STREAM_GAIN: u32 = 56;

/// Recompute the pitch ratio if it moved, advance the position, and return the whole frames consumed
/// (`sub_82B2DAC8`). `voice` is `r3`, `stream` `r4`, `frames` `r6`.
pub fn advance_pitch(g: &mut Guest, voice: u32, stream: u32, frames: u32) -> Result<u64> {
    let format = g.u32(stream.wrapping_add(40))?; // lwz r11,40(r4)
    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();
    let requested = fp::load_single(g, voice.wrapping_add(VOICE_REQUESTED))?;
    let scale = fp::load_single(g, voice.wrapping_add(VOICE_SCALE))?;
    let previous = fp::load_single(g, voice.wrapping_add(VOICE_PREVIOUS))?;
    let source_rate = fp::load_single(g, format.wrapping_add(12))?;
    let ratio = fp::mul_single(fp::div_single(requested, source_rate), scale); // fdivs ; fmuls

    if previous != ratio {
        let mut scaled = fp::mul_single(ratio, fp::load_single(g, RATIO_SCALE)?);
        let zero = fp::load_single(g, crate::leaves::ZERO_CELL)?;
        let half = fp::load_single(g, ROUND_HALF)?;
        // blt on the compare with zero: round half away from zero, then truncate.
        scaled = if scaled < zero { fp::sub_single(scaled, half) } else { fp::add_single(scaled, half) };
        let mut step = fp::fctiwz_low_word(scaled);
        if step as i32 > STEP_LIMIT {
            step = STEP_LIMIT as u32;
            let ceiling = fp::load_single(g, RATIO_CEILING)?;
            fp::store_single(g, voice.wrapping_add(VOICE_RATIO), ceiling)?; // stfs f0,56(r3)
        } else {
            fp::store_single(g, voice.wrapping_add(VOICE_RATIO), ratio)?; // stfs f12,56(r3)
        }
        fp::store_single(g, voice.wrapping_add(VOICE_PREVIOUS), ratio)?; // stfs f12,60(r3)
        g.set_u32(voice.wrapping_add(VOICE_STEP), step)?; // stw r11,68(r3)
    }

    // The tail runs on both paths and re-reads the step and the ratio.
    let stored_step = g.u32(voice.wrapping_add(VOICE_STEP))?;
    let stored_ratio = fp::load_single(g, voice.wrapping_add(VOICE_RATIO))?;
    let fraction = g.u32(voice.wrapping_add(VOICE_FRACTION))?;
    // mullw -- the full 64-bit product of the sign-extended words.
    let advanced = (i64::from(stored_step as i32) * i64::from(frames as i32)) as u64;
    g.set_u16(voice.wrapping_add(VOICE_FRAMES), frames as u16)?; // sth r6,78(r3)
    let bias = u64::from(g.u8(voice.wrapping_add(VOICE_BIAS))?);
    let gain = fp::load_single(g, stream.wrapping_add(STREAM_GAIN))?;
    let credit = u64::from(g.u8(voice.wrapping_add(VOICE_CREDIT))?);
    fp::store_single(g, stream.wrapping_add(STREAM_GAIN), fp::mul_single(stored_ratio, gain))?;
    let position = advanced.wrapping_add(u64::from(fraction));
    let whole = u64::from((position as u32) >> 16); // rlwinm r5,r6,16,16,31
    let remaining = whole.wrapping_sub(bias).wrapping_add(credit);
    let sign = u64::from((remaining as u32) >> 31);
    let mask = sign.wrapping_sub(1); // all ones unless the low word is negative
    Ok(mask & remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x4000_0000;
    const VOICE: u32 = BASE + 0x100;
    const STREAM: u32 = BASE + 0x200;
    const FORMAT: u32 = BASE + 0x300;
    const POISON: u32 = 0xDEAD_BEEF;

    fn guest(requested: f32, rate: f32, scale: f32, previous: f32) -> Guest {
        let mut g = Guest::single(BASE, 0x1000);
        for (addr, v) in [(RATIO_SCALE, 65536.0f32), (ROUND_HALF, 0.5), (RATIO_CEILING, 4.0), (crate::leaves::ZERO_CELL, 0.0)] {
            g.put(addr, v.to_bits().to_be_bytes().to_vec());
        }
        g.set_u32(STREAM + 40, FORMAT).unwrap();
        g.set_u32(FORMAT + 12, rate.to_bits()).unwrap();
        g.set_u32(STREAM + STREAM_GAIN, 1.0f32.to_bits()).unwrap();
        g.set_u32(VOICE + VOICE_REQUESTED, requested.to_bits()).unwrap();
        g.set_u32(VOICE + VOICE_SCALE, scale.to_bits()).unwrap();
        g.set_u32(VOICE + VOICE_PREVIOUS, previous.to_bits()).unwrap();
        g.set_u32(VOICE + VOICE_RATIO, POISON).unwrap();
        g.set_u32(VOICE + VOICE_STEP, POISON).unwrap();
        g
    }

    #[test]
    fn a_new_ratio_becomes_a_rounded_step_and_advances_the_position() {
        // 24000 / 48000 * 1.0 = 0.5, so the step is 0.5 * 65536 + 0.5 truncated = 32768.
        let mut g = guest(24_000.0, 48_000.0, 1.0, -1.0);
        let consumed = advance_pitch(&mut g, VOICE, STREAM, 256).unwrap();
        assert_eq!(g.u32(VOICE + VOICE_STEP).unwrap(), 32768);
        assert_eq!(g.f32(VOICE + VOICE_RATIO).unwrap(), 0.5);
        assert_eq!(g.f32(VOICE + VOICE_PREVIOUS).unwrap(), 0.5);
        assert_eq!(g.u16(VOICE + VOICE_FRAMES).unwrap(), 256);
        assert_eq!(consumed, 128, "256 frames at half speed consume 128 source frames");
        assert_eq!(g.f32(STREAM + STREAM_GAIN).unwrap(), 0.5, "the gain is scaled by the ratio");
    }

    #[test]
    fn an_unchanged_ratio_skips_the_recompute_and_its_stores() {
        let mut g = guest(24_000.0, 48_000.0, 1.0, 0.5);
        g.set_u32(VOICE + VOICE_RATIO, 0.25f32.to_bits()).unwrap(); // what the tail will read
        g.set_u32(VOICE + VOICE_STEP, 16384).unwrap();
        let consumed = advance_pitch(&mut g, VOICE, STREAM, 256).unwrap();
        assert_eq!(g.u32(VOICE + VOICE_STEP).unwrap(), 16384, "not rewritten");
        assert_eq!(g.f32(VOICE + VOICE_RATIO).unwrap(), 0.25, "not rewritten");
        assert_eq!(consumed, 64, "the tail uses the stored step, re-read from memory");
        assert_eq!(g.f32(STREAM + STREAM_GAIN).unwrap(), 0.25, "and the stored ratio");
    }

    #[test]
    fn the_step_rounds_half_away_from_zero() {
        // A negative scale makes a negative ratio: -0.5 * 65536 = -32768 exactly, minus the half is
        // -32768.5, truncated toward zero is -32768. Take a ratio whose scaled value is x.5 to see it:
        // ratio 1/131072 scales to exactly 0.5, and half away from zero makes that 1, not 0.
        let mut g = guest(1.0, 131_072.0, 1.0, -1.0);
        advance_pitch(&mut g, VOICE, STREAM, 1).unwrap();
        assert_eq!(g.u32(VOICE + VOICE_STEP).unwrap(), 1, "0.5 rounds up to 1");
        let mut g = guest(1.0, 131_072.0, -1.0, 1.0);
        advance_pitch(&mut g, VOICE, STREAM, 1).unwrap();
        assert_eq!(g.u32(VOICE + VOICE_STEP).unwrap() as i32, -1, "-0.5 rounds down to -1");
    }

    #[test]
    fn a_saturated_step_stores_the_ceiling_ratio_instead() {
        // A ratio of 8 scales to 2^19, past the 2^18 limit.
        let mut g = guest(8.0, 1.0, 1.0, -1.0);
        advance_pitch(&mut g, VOICE, STREAM, 1).unwrap();
        assert_eq!(g.u32(VOICE + VOICE_STEP).unwrap(), STEP_LIMIT as u32);
        assert_eq!(g.f32(VOICE + VOICE_RATIO).unwrap(), 4.0, "the pool ceiling, not 8");
        assert_eq!(g.f32(VOICE + VOICE_PREVIOUS).unwrap(), 8.0, "while +60 keeps the real ratio");
    }

    #[test]
    fn the_result_clamps_at_zero_when_the_bias_outweighs_the_frames() {
        let mut g = guest(24_000.0, 48_000.0, 1.0, -1.0);
        g.set_u8(VOICE + VOICE_BIAS, 200).unwrap();
        g.set_u8(VOICE + VOICE_CREDIT, 3).unwrap();
        assert_eq!(advance_pitch(&mut g, VOICE, STREAM, 256).unwrap(), 0, "128 - 200 + 3 clamps");
        let mut g = guest(24_000.0, 48_000.0, 1.0, -1.0);
        g.set_u8(VOICE + VOICE_BIAS, 20).unwrap();
        g.set_u8(VOICE + VOICE_CREDIT, 3).unwrap();
        assert_eq!(advance_pitch(&mut g, VOICE, STREAM, 256).unwrap(), 111, "128 - 20 + 3");
    }

    #[test]
    fn the_pool_addresses_come_from_the_lis_immediates() {
        assert_eq!(RATIO_SCALE, 0x820B_0000 + 16660);
        assert_eq!(ROUND_HALF, 0x820A_0000 - 26788);
        assert_eq!(RATIO_CEILING, crate::dsp::gain_ramp::STEP_SCALE, "the same cell, reached twice");
    }
}
