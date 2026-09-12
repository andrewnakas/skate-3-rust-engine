//! The guest's scalar floating-point idioms, as the lifted PowerPC performs them.
//!
//! Nothing here is a port of a guest function. These are the recurring *instruction* shapes
//! that the verified C++ bodies spell out inline — `lfs`, `stfs`, `extsw`/`fcfid`/`frsp`, the
//! single-rounded arithmetic forms, and `fctiwz` — factored out so that every op in `eval`
//! rounds in the same places the original does. Each helper is named after the instruction
//! sequence it stands for, and none of them is allowed to be "simplified" into a plain `f32`
//! computation: the guest holds intermediates in 64-bit FPRs and narrows only where a `.s`
//! form appears, which is not the same thing as computing in `f32`.
//!
//! **Flush-to-zero.** The bodies these came from call
//! `ctx.fpscr.disableFlushModeUnconditional()` before each float group, which writes MXCSR
//! `0x0000`: round-to-nearest, denormals preserved. Rust's `f32`/`f64` operators run under the
//! host's default `0x1F80`, which differs only in that it *masks* the FP exceptions the guest
//! leaves unmasked. Masking changes whether a trap fires, never the value produced, so the
//! results here match and the FPSCR calls have no Rust counterpart. (The recomp's own hooks do
//! have to care — an unmasked SIGFPE inside a guest call killed the first capture tap.)

use crate::{Guest, Result};

/// `lfs`: the 32-bit word reinterpreted as a single, widened to double.
///
/// Widening quiets a signalling NaN on the guest and in Rust alike; the payload it leaves is
/// the hardware's in both cases and is not pinned here.
#[inline]
pub fn single_from_bits(bits: u32) -> f64 {
    f32::from_bits(bits) as f64
}

/// `stfs`: round the double to single and take its bits.
#[inline]
pub fn single_to_bits(value: f64) -> u32 {
    (value as f32).to_bits()
}

/// `lfs` from guest memory.
#[inline]
pub fn load_single(g: &Guest, ea: u32) -> Result<f64> {
    Ok(single_from_bits(g.u32(ea)?))
}

/// `stfs` to guest memory.
#[inline]
pub fn store_single(g: &mut Guest, ea: u32, value: f64) -> Result<()> {
    g.set_u32(ea, single_to_bits(value))
}

/// `extsw ; std ; lfd ; fcfid ; frsp`: a 32-bit field sign-extended, converted exactly to
/// double, then rounded to single and kept in a double.
///
/// The red-zone spill the original uses to move the word into an FPR is a bit copy and has no
/// effect on the value, so it is not reproduced. The `frsp` does: word magnitudes above 2^24
/// lose low bits here, and dropping the narrowing would make this op disagree with the guest
/// on ordinary sample counts.
#[inline]
pub fn word_to_single(word: u32) -> f64 {
    let wide = (word as i32) as i64; // extsw
    let converted = wide as f64; // fcfid
    (converted as f32) as f64 // frsp
}

/// `fadds`: the double sum rounded to single.
#[inline]
pub fn add_single(a: f64, b: f64) -> f64 {
    ((a + b) as f32) as f64
}

/// `fsubs`.
#[inline]
pub fn sub_single(a: f64, b: f64) -> f64 {
    ((a - b) as f32) as f64
}

/// `fmuls`.
#[inline]
pub fn mul_single(a: f64, b: f64) -> f64 {
    ((a * b) as f32) as f64
}

/// `fdivs`.
#[inline]
pub fn div_single(a: f64, b: f64) -> f64 {
    ((a / b) as f32) as f64
}

/// `fmadds`: one fused double-precision multiply-add, then rounded to single.
///
/// Fused because the lifted line is `fmadds`. A `mul_single` followed by an `add_single` would
/// round twice and is a different function; the operand order is kept for NaN propagation.
#[inline]
pub fn fmadd_single(a: f64, b: f64, c: f64) -> f64 {
    (a.mul_add(b, c) as f32) as f64
}

/// `fctiwz f13,fX ; stfd f13,-k(r1) ; lwz rN,-k+4(r1)`: truncate toward zero into a word, spill
/// big-endian, read the low word back.
///
/// Written branch for branch because the guest's conversion and Rust's saturating `as i32`
/// disagree on NaN: RexGlue's lowering yields the x86 "integer indefinite" `0x80000000`, while
/// `f64 as i32` in Rust yields 0. Everywhere else the two agree, and
/// `tests::naive_cast_agrees_except_on_nan` pins that — the saturation Rust performs below
/// `i32::MIN` produces the same `0x80000000` the hardware's indefinite value has, and the
/// `value > i32::MAX` arm covers the other end before the cast can be reached.
///
/// The result is the *low word only*: the original's `lwz` takes four bytes out of an eight-byte
/// spill, so the caller sees it zero-extended, never sign-extended.
#[inline]
pub fn fctiwz_low_word(value: f64) -> u32 {
    if value.is_nan() {
        0x8000_0000
    } else if value > i32::MAX as f64 {
        i32::MAX as u32
    } else {
        (value as i32) as u32
    }
}

/// `rlwinm rA,rS,SH,MB,ME` as RexGlue lowers it: the low word is rotated into *both* halves of
/// a 64-bit value before the mask is applied.
///
/// Written out rather than treated as a 32-bit shift because the two are only equal for masks
/// that stay inside the low word. `sub_82B1C6C8`'s divide-by-100 uses it with a 27-bit rotate
/// and a 27-bit mask, where the doubled operand is what feeds the result.
#[inline]
pub fn rlwinm(value: u64, sh: u32, mask: u64) -> u64 {
    let lo = value & 0xFFFF_FFFF;
    (lo | (lo << 32)).rotate_left(sh) & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_cast_agrees_except_on_nan() {
        // The one case that forces the branch-for-branch form.
        assert_eq!(fctiwz_low_word(f64::NAN), 0x8000_0000);
        assert_eq!((f64::NAN as i32) as u32, 0, "the naive cast, which would diverge");

        // Both ends of the range, where saturation happens to reproduce the hardware.
        assert_eq!(fctiwz_low_word(f64::INFINITY), i32::MAX as u32);
        assert_eq!(fctiwz_low_word(f64::NEG_INFINITY), 0x8000_0000);
        assert_eq!(fctiwz_low_word(3e9), i32::MAX as u32);
        assert_eq!(fctiwz_low_word(-3e9), 0x8000_0000);
        assert_eq!(fctiwz_low_word(i32::MAX as f64), 0x7FFF_FFFF);
        assert_eq!(fctiwz_low_word(i32::MIN as f64), 0x8000_0000);

        // Truncation toward zero, not rounding: this is what makes the callers add or subtract
        // a half before converting.
        assert_eq!(fctiwz_low_word(1.9), 1);
        assert_eq!(fctiwz_low_word(-1.9), (-1i32) as u32);
        assert_eq!(fctiwz_low_word(-0.5), 0);
        assert_eq!(fctiwz_low_word(0.0), 0);
    }

    #[test]
    fn word_to_single_narrows_where_the_guest_narrows() {
        assert_eq!(word_to_single(0), 0.0);
        assert_eq!(word_to_single(48_000), 48_000.0);
        // Sign extension, not zero extension: the field is a signed word.
        assert_eq!(word_to_single(0xFFFF_FFFF), -1.0);
        assert_eq!(word_to_single(0x8000_0000), i32::MIN as f64);

        // The frsp matters past 2^24. 16_777_217 is not representable as f32 and rounds to
        // even; a version that skipped the narrowing would return the exact integer.
        assert_eq!(word_to_single(16_777_217), 16_777_216.0);
        assert_ne!(16_777_217i32 as f64, 16_777_216.0, "the un-narrowed value differs");
    }

    #[test]
    fn single_rounded_forms_round_once_at_the_end() {
        // The plain forms: a double operation whose result is narrowed. The distinguishing case
        // for "rounds once" is the fused multiply-add below; these pin the shape.
        let tiny = 2f64.powi(-25);
        assert_eq!(add_single(1.0, tiny), 1.0);
        assert_eq!(sub_single(1.0, tiny), 1.0);
        assert_eq!(mul_single(1.5, 2.0), 3.0);
        assert_eq!(div_single(1.0, 3.0), (1.0f32 / 3.0f32) as f64);

        // fmadds is fused: the product is not rounded before the add. Multiplying two f32-exact
        // values whose product needs 48 bits and adding its negation leaves 0 only if the
        // product was kept intact.
        let a = (1.0f32 + f32::EPSILON) as f64;
        let b = (1.0f32 - f32::EPSILON) as f64;
        assert_eq!(fmadd_single(a, b, -(a * b)), 0.0);
        // The unfused form rounds a*b to single first and cannot cancel.
        assert_ne!(add_single(mul_single(a, b), -(a * b)), 0.0);
    }

    #[test]
    fn lfs_and_stfs_are_a_bit_round_trip_for_singles() {
        let bits = 0x3F80_0000; // 1.0f
        assert_eq!(load_single_bits_round_trip(bits), bits);
        assert_eq!(single_from_bits(bits), 1.0);
        assert_eq!(single_to_bits(1.0), bits);
        // A double that does not fit a single is rounded by the store, as `stfs` does.
        assert_eq!(single_to_bits(1.0 + 2f64.powi(-30)), bits);
    }

    fn load_single_bits_round_trip(bits: u32) -> u32 {
        single_to_bits(single_from_bits(bits))
    }

    #[test]
    fn rlwinm_doubles_the_low_word_before_rotating() {
        // The divide-by-100 shape sub_82B1C6C8 uses: rotate 27, mask 27 bits, which for an
        // operand inside the low word is a logical right shift by 5.
        const MASK: u64 = 0x7FF_FFFF;
        assert_eq!(rlwinm(0x20, 27, MASK), 0x20 >> 5);
        assert_eq!(rlwinm(0xFFFF_FFFF, 27, MASK), 0xFFFF_FFFFu64 >> 5);

        // The doubling is load-bearing, not decoration: rotating the un-doubled 64-bit value by
        // 27 moves the low word's bits clear of the mask and yields zero.
        assert_eq!(0x20u64.rotate_left(27) & MASK, 0);

        // Only the low word participates, so bits above 31 are discarded rather than rotated in.
        assert_eq!(rlwinm(0xDEAD_0000_0000_0020, 27, MASK), 0x20 >> 5);
    }
}
