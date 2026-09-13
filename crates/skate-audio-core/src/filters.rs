//! The per-channel filter stages: a low-pass, a high-pass, and the coefficient builder they share.
//!
//! Both stages take the same filter object in `r3` and the same stream in `r4`, normalise a cutoff
//! out of the object's `+52` and the stream's sample rate, and then either run
//! [`crate::dsp::biquad`] over 256 frames of every channel and swap the stream's descriptor pair, or
//! **bypass** — clear the filter history and leave the pair alone, so the caller keeps reading the
//! untouched input block. They are mirror images of one another and they are not symmetric, which is
//! the main thing this file exists to make visible.
//!
//! | function | guest | `docs/ports.md` | lifted lines | calls/boot | calls/play |
//! |---|---|---|---|---|---|
//! | [`lowpass_stage`] | `sub_82B27E20` | verified | 214 | 183,427 | 291,143 |
//! | [`highpass_stage`] | `sub_82B26568` | verified | 287 | 171,012 | 276,388 |
//! | [`build_lowpass_coefficients`] | `sub_82B43CC0` | verified | 111 | 4,400 | 3,226 |
//!
//! Every one of those `.inc` headers leads with `// STATUS: verified` and `docs/ports.md` agrees on
//! all three — checked before translating, because a header reading `thin`, `partial` or `gate-`
//! would not be a reference at all.
//!
//! `build_lowpass_coefficients` is not in the same call-count league as the other two and it is here
//! because [`lowpass_stage`] **calls** it: the low-pass builds its five coefficients in a separate
//! guest function, and the high-pass builds the same five inline. Porting the stages without it would
//! have left the low-pass's recompute path as a stub, which is the only part of it a test can drive.
//!
//! ## What the green here means
//!
//! **Unit-tested against a verified reference**, the crate README's second kind of green. Each C++
//! body was compared call-for-call against the original under the shadow harness, on the real inputs
//! those call counts come from, at zero divergence. The Rust has no recorded vectors of its own, and
//! for these three it cannot have any yet — see the [`Trig`] note below. A fault here is a
//! transcription error rather than a misreading of the engine; it is not a number.
//!
//! ## Nothing in `docs/rw_audio_structs.h` names this object
//!
//! Every offset below is the raw offset plus the use the two lifted bodies make of it. No field name
//! here is recovered from RTTI or from plug-in metadata, and none should be read as one. The
//! `+56`/`+184` pair does carry one real constraint: `(184 - 56) / 16 == 8`, so the state array holds
//! at most eight channels and a ninth would run into the coefficients. Both C++ `Windows()` builders
//! decline a channel count above eight for that reason — a corrupted object rather than a case to
//! compare — and [`MAX_CHANNELS`] records it without enforcing it, because the original does not.
//!
//! ## The two stages are mirrors, and the mirror is not exact
//!
//! | | [`lowpass_stage`] | [`highpass_stage`] |
//! |---|---|---|
//! | bypasses when | `!(cutoff < ceiling)` | `!(cutoff > floor)` |
//! | clears history when the cache was | `< ceiling` | `> floor` |
//! | then clamps to | the **floor**, from below | the **ceiling**, from above |
//! | coefficients | `sub_82B43CC0` | built inline |
//! | numerator | `1 - cos` | `1 + cos` |
//! | `b1` | `+(1-cos)/a0` | `-(1+cos)/a0`, through an `fneg` |
//!
//! Both bypass tests are **unordered**: a NaN cutoff fails `<` and fails `>`, so both stages bypass
//! on it. Both clamps are strict, so a cutoff exactly on its bound is left alone.
//!
//! Reading the four coefficient cells out of the validated image dump makes the arithmetic legible
//! rather than merely transcribed. They are `0.5`, `1.0`, `-2.0` and `2.0` — three of which this crate
//! already names from [`crate::spatial`]'s own reading of the same dump — so with
//! `alpha = 0.5·sin w` and `a0 = 1 + alpha` both bodies are the textbook RBJ biquad at Q = 1, every
//! coefficient divided through by `a0`:
//!
//! ```text
//! a1 = -2cos/a0   a2 = (1-alpha)/a0
//! low-pass    b0 = b2 = (1-cos)/(2a0)   b1 =  (1-cos)/a0
//! high-pass   b0 = b2 = (1+cos)/(2a0)   b1 = -(1+cos)/a0
//! ```
//!
//! ## `Trig`, and what it costs
//!
//! Both recompute paths reach the image's sine and cosine, `sub_82F4DED0` and `sub_82F4DFB0`, which
//! **have no port in either language** — outside the 216 audio-thread functions the sweep covered, so
//! there is no `.inc` to transcribe. They are therefore a [`Trig`] parameter, exactly as in
//! [`crate::spatial`], whose default [`crate::mathlib::Unported`] returns an `Err` naming the guest
//! address rather than a plausible number.
//!
//! The consequence is narrower here than in `spatial`, and worth stating precisely: a call whose
//! cutoff equals the cached cutoff never asks for a sine, so it runs to completion under `Unported`
//! and **is** replayable. Only a call that recomputes is not. The same applies to the whole bypass
//! path, which never reaches trigonometry at all.
//!
//! ## Reproduced rather than fixed
//!
//! - **The channel count is re-read from `+42` at the bottom of every loop**, in all three loops here,
//!   so a filter pass that stored over the object's own header would change its own trip count. Both
//!   C++ `Windows()` builders decline a layout where that could happen, so nothing is known about what
//!   the guest does with it; the reload is kept because the original has it.
//! - **The state pointer walks on the callee's `r3`.** `addi r3,r3,16` is applied to whatever
//!   `sub_82B43AF8` left in `r3`, and that function never writes it — so the walk depends on the
//!   callee returning its first argument untouched. [`crate::dsp::biquad`] has no return value to read,
//!   so the walk is written as `state + 16` with this note attached rather than as a register read.
//! - **`sub_82B43CC0` computes `b2` with a second `fdivs` of the same two operands** instead of copying
//!   `b0`. Two single-rounded divides of identical operands agree, so this is invisible in value; it is
//!   written out because a `fmuls` by `0.5` or a copy would not be the same instruction.
//! - **`sub_82B26568` stores its five coefficients in the order 184, 188, 192, 200, 196**, which is
//!   not ascending. Kept.
//!
//! ## What no test here can catch
//!
//! Two things, each measured the same way the crate README's other entries were — the change made,
//! the whole suite re-run, all still green — rather than argued from the arithmetic.
//!
//! - **The coefficient store order.** The five stores go to five **distinct** words with five distinct
//!   values, so a single-threaded comparison cannot order them in either direction: reversing `+196`
//!   and `+200` leaves every byte of the object identical. Written in the original's order because the
//!   original has it; `b1` and `b2` are separately asserted to hold the right values, which is all a
//!   test here can establish.
//! - **The `frsp` on the *sine*, in all three bodies.** Its only consumer is `alpha = fmuls(sine, 0.5)`
//!   and scaling by a power of two commutes with rounding to single, so `(x·0.5)→f32` equals
//!   `(x→f32)·0.5` for every non-denormal `x`. Removing that one `frsp` leaves the suite green. The
//!   `frsp` on the **cosine** is a different matter and is pinned, because its consumer is
//!   `fsubs(1, cos)` / `fadds(cos, 1)`, where a tail below the single's last bit survives as a
//!   non-zero difference — see `the_cosines_double_tail_is_discarded_by_the_frsp`.
//!
//! ## Not reproduced, and why
//!
//! Neither stage's register spill is reproduced: the originals save `f29`-`f31` and `r26`-`r31` because
//! their callees may clobber the volatile file, and the C++ ports keep the entry values in locals for
//! the same reason. There is no register file here, so nothing is written to memory in any version.
//!
//! `sub_82B43CC0`'s `stwu r1,-112(r1)` is **also** not reproduced, and that one is a decision rather
//! than an absence. The C++ port does reproduce it, because its two callees are real guest bodies whose
//! spills land below `r1` and would otherwise overwrite this function's own. With the trigonometry as a
//! parameter there is no callee to make room for — the same reasoning `spatial::add_angular` records
//! for its own `stwu r1,-160(r1)`.

use crate::mathlib::Trig;
use crate::vmx::Fpscr;
use crate::{Guest, Result, dsp, fp, gains};

// ------------------------------------------------------------------------------- the filter object

/// `u8` at `+42` — the channel count, **re-read** at the bottom of every loop.
pub const CHANNEL_COUNT: u32 = 42;
/// `f32` at `+52` — the numerator of the normalised cutoff.
pub const CUTOFF_INPUT: u32 = 52;
/// 16 bytes per channel, `{x1, x2, y1, y2}`: exactly what [`crate::dsp::biquad`] carries.
pub const FILTER_STATE: u32 = 56;
/// Five singles at `+184`, in the order `sub_82B43AF8` consumes them.
pub const COEFFICIENTS: u32 = 184;
/// `f32` at `+204` — the cutoff the coefficients at [`COEFFICIENTS`] were built from.
pub const CACHED_CUTOFF: u32 = 204;
/// `(184 - 56) / 16` — the state array's capacity, not a bound either original enforces.
pub const MAX_CHANNELS: u32 = 8;

const _: () = assert!((COEFFICIENTS - FILTER_STATE) / 16 == MAX_CHANNELS, "eight state slots");
const _: () = assert!(CACHED_CUTOFF == COEFFICIENTS + 20, "the cache sits past the five singles");

// --------------------------------------------------------------------------------- the stream

/// `rw_ptr` at `+28` — the descriptor read this call, and written on the swap.
pub const STREAM_BUFFER_A: u32 = 28;
/// `rw_ptr` at `+32` — the descriptor written this call.
pub const STREAM_BUFFER_B: u32 = 32;
/// `rw_ptr` at `+40` — the format block. Dereferenced unconditionally, before any store.
pub const STREAM_FORMAT: u32 = 40;
/// `f32` at `+12` of the format block — the sample rate the cutoff is normalised by.
pub const FORMAT_SAMPLE_RATE: u32 = 12;

const _: () = assert!(STREAM_BUFFER_B == STREAM_BUFFER_A + 4, "the pair is swapped as two words");

// ------------------------------------------------------------------------------- the descriptor

/// `u32` at `+4` of a buffer descriptor — channel 0's float array.
pub const DESC_BASE: u32 = 4;
/// `u16` at `+14` — singles between one channel and the next.
pub const DESC_STRIDE: u32 = 14;

// The same descriptor `gains.rs` walks, so its `mullw`/`rlwinm`/`add` chain is reused rather than
// written a fourth time. These two assertions are what makes the reuse legitimate: if either offset
// ever disagreed, the shared helper would be reading a different structure.
const _: () = assert!(DESC_BASE == gains::BUFFER_BASE, "the descriptor gains.rs walks");
const _: () = assert!(DESC_STRIDE == gains::CHANNEL_STRIDE);

/// `li r7,256` — both stages filter a fixed 256-frame block.
pub const BLOCK_FRAMES: u32 = 256;

// ------------------------------------------------------------------------ the five coefficients

// The offsets `dsp::biquad` reads them back from. Asserted rather than re-stated, because the whole
// point of writing coefficients here is that that body consumes them.
const _: () = assert!(dsp::biquad::COEFF_A1 == 0);
const _: () = assert!(dsp::biquad::COEFF_A2 == 4);
const _: () = assert!(dsp::biquad::COEFF_B0 == 8);
const _: () = assert!(dsp::biquad::COEFF_B1 == 12);
const _: () = assert!(dsp::biquad::COEFF_B2 == 16);
/// Five singles, `{a1, a2, b0, b1, b2}`.
pub const COEFFICIENT_BYTES: u32 = 20;

// ------------------------------------------------------------------------------- the rodata cells

/// `((imm & 0xFFFF) << 16)` — the `lis` half of a constant's address, **computed**.
///
/// Never read off the disassembly by eye: one misread digit in `sub_82B2FE00` cost this project its
/// first shadow divergence, and `CLAUDE.md` records a second wrong value reaching a committed note.
const fn lis(imm: i32) -> u32 {
    ((imm as u32) & 0xFFFF) << 16
}

const _: () = assert!(lis(-32245) == 0x820B_0000, "lis r11,-32245");
const _: () = assert!(lis(-32208) == 0x8230_0000, "lis r11,-32208");
const _: () = assert!(lis(-32234) == 0x8216_0000, "lis r9,-32234");
const _: () = assert!(lis(-32246) == 0x820A_0000, "lis r11,-32246");
const _: () = assert!(lis(-32206) == 0x8232_0000, "lis r10,-32206");
const _: () = assert!(lis(-32247) == 0x8209_0000, "lis r9,-32247");
const _: () = assert!(lis(-32250) == 0x8206_0000, "lis r8,-32250");

/// `lis -32208 ; addi -31232` — the audio pool [`crate::dsp::biquad`] also names.
const POOL: u32 = lis(-32208).wrapping_add(-31232i32 as u32);
const _: () = assert!(POOL == 0x822F_8600, "lis -32208 ; addi -31232");
const _: () = assert!(POOL == dsp::biquad::POOL, "the same pool the kernel loads its bias from");

/// `lis -32245 ; lfs 16668` — multiplies the normalised `numerator / rate` back up.
pub const CUTOFF_SCALE: u32 = lis(-32245).wrapping_add(16668);
/// `lfs 2172` off [`POOL`] — the low bound. [`lowpass_stage`] clamps **up** to it; the high-pass
/// **bypasses** at or below it.
pub const CUTOFF_FLOOR: u32 = POOL.wrapping_add(2172);
/// `lfs 2176` off [`POOL`] — the high bound, with the two roles the other way round.
pub const CUTOFF_CEILING: u32 = POOL.wrapping_add(2176);
/// `lis -32234 ; lfs 23056` — the single both bypass paths clear the filter history with.
pub const ZERO_SINGLE: u32 = lis(-32234).wrapping_add(23056);
/// `lfs 432` off [`POOL`] — read by the kernel, not by either body here. Named so that the read
/// spans in this module's documentation account for it.
pub const KERNEL_BIAS: u32 = POOL.wrapping_add(432);

/// `lis -32246 ; lfs -26788` — measured **0.5**. `alpha = 0.5 · sin w`.
pub const HALF_SINGLE: u32 = lis(-32246).wrapping_add(-26788i32 as u32);
/// `lis -32206 ; lfs -22460` — measured **1.0**. The `1` in `a0 = 1 + alpha` and in `1 ∓ cos`.
pub const ONE_SINGLE: u32 = lis(-32206).wrapping_add(-22460i32 as u32);
/// `lis -32247 ; lfs 16760` — measured **-2.0**. The `-2` in `a1 = -2cos/a0`.
pub const MINUS_TWO_SINGLE: u32 = lis(-32247).wrapping_add(16760);
/// `lis -32250 ; lfs 3152` — measured **2.0**. The `2` in `b0 = num/(2·a0)`.
pub const TWO_SINGLE: u32 = lis(-32250).wrapping_add(3152);

const _: () = assert!(CUTOFF_SCALE == 0x820B_411C);
const _: () = assert!(CUTOFF_FLOOR == 0x822F_8E7C, "lis -32208 ; addi -31232 ; lfs 2172");
const _: () = assert!(CUTOFF_CEILING == 0x822F_8E80, "lfs 2176");
const _: () = assert!(CUTOFF_CEILING == CUTOFF_FLOOR + 4, "two adjacent words");
const _: () = assert!(ZERO_SINGLE == 0x8216_5A10, "lis -32234 ; lfs 23056");
const _: () = assert!(KERNEL_BIAS == 0x822F_87B0);
const _: () = assert!(KERNEL_BIAS == dsp::biquad::DENORM_BIAS, "the kernel's own bias cell");
const _: () = assert!(HALF_SINGLE == 0x8209_975C, "lis -32246 ; lfs -26788");
const _: () = assert!(ONE_SINGLE == 0x8231_A844, "lis -32206 ; lfs -22460");
const _: () = assert!(MINUS_TWO_SINGLE == 0x8209_4178, "lis -32247 ; lfs 16760");
const _: () = assert!(TWO_SINGLE == 0x8206_0C50, "lis -32250 ; lfs 3152");

// Three of the five are cells this crate has already read out of the validated image dump, from two
// other modules. Asserting the addresses agree is what turns "0.5, 1.0, 0.0" from an inference about
// this file's constants into the same measurement `spatial.rs` records.
const _: () = assert!(ZERO_SINGLE == crate::spatial::ZERO_SINGLE, "0.0f, measured");
const _: () = assert!(ONE_SINGLE == crate::spatial::ONE_SINGLE, "1.0f, measured");
const _: () = assert!(HALF_SINGLE == crate::spatial::HALF_SINGLE, "0.5f, measured");

// ================================================= sub_82B43CC0: the low-pass coefficients

/// `sub_82B43CC0` — build the five normalised low-pass biquad coefficients from one angle.
///
/// Arguments, by register: `coefficients` is `r3` (addressed through the low word, `mr r31,r3`) and
/// `angle` is `f1`, in radians. There is no return value (`kReturnNone`); `r3` on exit still holds
/// the pointer.
///
/// **Writes** exactly [`COEFFICIENT_BYTES`] at `coefficients`, in ascending order, each single stored
/// immediately after it is computed. Reads [`HALF_SINGLE`], [`ONE_SINGLE`], [`MINUS_TWO_SINGLE`] and
/// [`TWO_SINGLE`], all four **live** through the guest map, plus whatever `trig` reads.
///
/// `b2` is computed by a **second** `fdivs` of the same two operands rather than copied from `b0` —
/// see the module note. Both call sites pass the interior of a filter object (`+184` from
/// [`lowpass_stage`], `+172` from `sub_82B2D5D4`) and neither reads a register result, so those twenty
/// bytes are the whole observable output.
pub fn build_lowpass_coefficients<T: Trig>(
    g: &mut Guest,
    trig: &mut T,
    coefficients: u32,
    angle: f64,
) -> Result<()> {
    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();

    // bl 0x82f4ded0 — sine, then cosine, each `frsp`'d before it is used. The narrowing is what
    // keeps the double-precision tail of the helper's answer out of everything below.
    let sine_raw = trig.sine(g, angle)?;
    fpscr.disable_flush_mode_unconditional(); // emitted on return, before the frsp
    let sine = fp::frsp(sine_raw); // frsp f30,f1
    let cosine_raw = trig.cosine(g, angle)?; // fmr f1,f31 ; bl 0x82f4dfb0
    fpscr.disable_flush_mode_unconditional();
    let cosine = fp::frsp(cosine_raw); // frsp f11,f1

    let half = fp::load_single(g, HALF_SINGLE)?; // lfs f0,-26788(r11)
    let alpha = fp::mul_single(sine, half); // fmuls f10,f30,f0
    let one = fp::load_single(g, ONE_SINGLE)?; // lfs f0,-22460(r10)
    let minus_two = fp::load_single(g, MINUS_TWO_SINGLE)?; // lfs f13,16760(r9)
    let two = fp::load_single(g, TWO_SINGLE)?; // lfs f12,3152(r8)
    let numerator = fp::sub_single(one, cosine); // fsubs f9,f0,f11 — the low-pass form, `1 - cos`
    let neg_two_cos = fp::mul_single(cosine, minus_two); // fmuls f8,f11,f13
    let a0 = fp::add_single(alpha, one); // fadds f7,f10,f0
    let one_minus_alpha = fp::sub_single(one, alpha); // fsubs f6,f0,f10
    let inv_a0 = fp::div_single(one, a0); // fdivs f5,f0,f7
    let two_a0 = fp::mul_single(a0, two); // fmuls f4,f7,f12

    let at = |k: u32| coefficients.wrapping_add(k);
    fp::store_single(g, at(dsp::biquad::COEFF_A1), fp::mul_single(neg_two_cos, inv_a0))?;
    fp::store_single(g, at(dsp::biquad::COEFF_A2), fp::mul_single(one_minus_alpha, inv_a0))?;
    fp::store_single(g, at(dsp::biquad::COEFF_B0), fp::div_single(numerator, two_a0))?;
    fp::store_single(g, at(dsp::biquad::COEFF_B1), fp::mul_single(numerator, inv_a0))?;
    // fdivs f13,f9,f4 — the same quotient a second time, not a copy of b0. See the module note.
    fp::store_single(g, at(dsp::biquad::COEFF_B2), fp::div_single(numerator, two_a0))
}

// ------------------------------------------------------------------------------ shared by the stages

/// `fdivs`, then `fmuls` — two single-rounded operations, **not** one FMA.
///
/// `numerator / rate * scale`, where the division's result is narrowed to a single before it is
/// multiplied. Written once because both stages open with the identical three instructions.
fn normalised_cutoff(g: &Guest, object: u32, format: u32) -> Result<f64> {
    let numerator = fp::load_single(g, object.wrapping_add(CUTOFF_INPUT))?; // lfs f0,52(r3)
    let rate = fp::load_single(g, format.wrapping_add(FORMAT_SAMPLE_RATE))?; // lfs f13,12(r10)
    let scale = fp::load_single(g, CUTOFF_SCALE)?;
    // fdivs f12,f0,f13 ; fmuls f31,f12,f13
    Ok(fp::mul_single(fp::div_single(numerator, rate), scale))
}

/// The bypass path's history clear: four singles per channel, walked `stfsu`-style.
///
/// The cursor starts at `object + 52` — the cutoff word, *not* the state array — and the four stores
/// per pass land at `+4`, `+8`, `+12` and then at the cursor after it advances by 16. That is what
/// puts channel 0's four words at `+56`…`+68` and channel `i`'s at `+56 + 16i`. Written as the
/// instruction pair forms it rather than as `FILTER_STATE + 16*i`, because the pre-increment is the
/// reason the array starts four bytes above the cursor's base.
///
/// The count is **reloaded** at the bottom of every pass, as the original does.
fn clear_history(g: &mut Guest, fpscr: &mut Fpscr, object: u32, mut channels: u32) -> Result<()> {
    if channels == 0 {
        return Ok(()); // beq cr6 — checked by the caller too, and by the loop's shape
    }
    let zero = fp::load_single(g, ZERO_SINGLE)?; // lfs f0,23056(r9)
    let mut cursor = object.wrapping_add(CUTOFF_INPUT); // addi r11,r3,52
    let mut index: u32 = 0; // li r10,0
    loop {
        fpscr.disable_flush_mode_unconditional();
        fp::store_single(g, cursor.wrapping_add(4), zero)?; // stfs f0,4(r11)
        index += 1; // addi r10,r10,1
        fp::store_single(g, cursor.wrapping_add(8), zero)?; // stfs f0,8(r11)
        fp::store_single(g, cursor.wrapping_add(12), zero)?; // stfs f0,12(r11)
        cursor = cursor.wrapping_add(16); // stfsu f0,16(r11): store at r11+16, then r11 = ea
        fp::store_single(g, cursor, zero)?;
        channels = g.u8(object.wrapping_add(CHANNEL_COUNT))? as u32; // lbz r9,42(r30)
        if !(index < channels) {
            return Ok(()); // cmplw cr6,r10,r9 ; blt
        }
    }
}

/// The filter pass and the descriptor swap, shared verbatim by both stages.
///
/// Both bodies emit these instructions identically, down to the order the two strides are loaded
/// before the two bases. `state` is kept as an `i64` because the original's is: `addi r3,r30,56` on a
/// 64-bit register, then `addi r3,r3,16` per channel.
fn filter_channels_and_swap(g: &mut Guest, object: u64, stream: u32) -> Result<()> {
    let object_low = object as u32;
    let mut channels = g.u8(object_low.wrapping_add(CHANNEL_COUNT))? as u32; // lbz r11,42(r30)
    let src_desc = g.u32(stream.wrapping_add(STREAM_BUFFER_A))?; // lwz r29,28(r26)
    let dst_desc = g.u32(stream.wrapping_add(STREAM_BUFFER_B))?; // lwz r28,32(r26)

    if channels != 0 {
        let coefficients = object.wrapping_add(COEFFICIENTS as u64); // addi r27,r30,184
        let mut state = (object as i64).wrapping_add(FILTER_STATE as i64); // addi r3,r30,56
        let mut index: u32 = 0; // li r31,0
        loop {
            let src_stride = g.u16(src_desc.wrapping_add(DESC_STRIDE))? as u32; // lhz 14(r29)
            let dst_stride = g.u16(dst_desc.wrapping_add(DESC_STRIDE))? as u32; // lhz 14(r28)
            let src_base = g.u32(src_desc.wrapping_add(DESC_BASE))?; // lwz 4(r29)
            let dst_base = g.u32(dst_desc.wrapping_add(DESC_BASE))?; // lwz 4(r28)
            // mullw ; rlwinm ; add — 64-bit product, 32-bit shift, 64-bit sum. The index cannot
            // exceed the u8 count that bounds the loop, so the `mullw`'s sign extension of it is the
            // identity here.
            let src_ptr = gains::channel_address(src_stride, index, src_base);
            let dst_ptr = gains::channel_address(dst_stride, index, dst_base);
            // bl 0x82b43af8 — reads 256 singles at r5, writes 256 at r4, rewrites four state words
            // at r3. r7 is 256, so the kernel never reaches its generic fallback.
            dsp::biquad::biquad(
                g,
                state as u64 as u32,
                dst_ptr as u32,
                src_ptr as u32,
                coefficients as u32,
                BLOCK_FRAMES,
            )?;
            channels = g.u8(object_low.wrapping_add(CHANNEL_COUNT))? as u32; // lbz r11,42(r30)
            index += 1; // addi r31,r31,1
            // addi r3,r3,16 on whatever the callee left in r3 — which sub_82B43AF8 never writes.
            // See the module note: this is a read of the callee's r3 in the original.
            state = state.wrapping_add(16);
            if !(index < channels) {
                break; // cmplw cr6,r31,r11 ; blt
            }
        }
    }

    // The ping-pong swap. Both loads happen before both stores, so a reordering here would write the
    // same descriptor into both slots.
    let new_a = g.u32(stream.wrapping_add(STREAM_BUFFER_B))?; // lwz r11,32(r26)
    let new_b = g.u32(stream.wrapping_add(STREAM_BUFFER_A))?; // lwz r10,28(r26)
    g.set_u32(stream.wrapping_add(STREAM_BUFFER_A), new_a)?; // stw r11,28(r26)
    g.set_u32(stream.wrapping_add(STREAM_BUFFER_B), new_b) // stw r10,32(r26)
}

// ============================================================== sub_82B27E20: the low-pass

/// `sub_82B27E20` — run the per-channel low-pass over a 256-frame block, or bypass it.
///
/// Arguments, by register: `object` is `r3` (the filter object) and `stream` is `r4`. Both arrive as
/// full 64-bit registers — the original keeps them in `r30`/`r26` and the state pointer it hands the
/// kernel is `r30 + 56` computed on the **whole** register — and both are addressed through their low
/// words. **Returns `1`** (`li r3,1`) on every path; `kReturnR3`, so that constant is the whole of the
/// register comparison.
///
/// **Writes**, as the union of both paths: the five coefficients and the cache at `+184`…`+207`; the
/// four state words of every channel at `+56 + 16i`; the two descriptor words at `stream + 28`; and,
/// on the filtering path only, 1 KB per channel through the destination descriptor. Reads `+42`,
/// `+52`, `+184`…`+207`, the format pointer at `stream + 40` and the rate at `+12` of it, both
/// descriptors, 1 KB per channel through the source descriptor, and the rodata cells
/// [`CUTOFF_SCALE`], [`CUTOFF_FLOOR`], [`CUTOFF_CEILING`], [`ZERO_SINGLE`], [`KERNEL_BIAS`] — plus,
/// through [`build_lowpass_coefficients`] on the recompute path, [`HALF_SINGLE`], [`ONE_SINGLE`],
/// [`MINUS_TWO_SINGLE`] and [`TWO_SINGLE`].
///
/// **Those last four, and `stream + 40` itself, are reads the C++ `Windows()` does not declare.**
/// That is recorded here rather than worked around: a recorded vector that reaches the recompute path
/// has none of those five cells in its read set and is unreplayable until one `spec.read` line per
/// cell is added. The high-pass twin declares the four coefficient cells, because it builds the
/// coefficients itself; the low-pass reaches them through a callee.
///
/// **The bypass is at the ceiling.** `!(cutoff < ceiling)` bypasses — which a NaN cutoff satisfies,
/// the compare being unordered — and then the descriptor pair is **not** swapped, so the caller keeps
/// reading the untouched input block. The history is cleared only when the *cached* cutoff was itself
/// below the ceiling, i.e. only when those words hold filter state worth clearing.
pub fn lowpass_stage<T: Trig>(
    g: &mut Guest,
    trig: &mut T,
    object: u64,
    stream: u64,
) -> Result<u64> {
    let object_low = object as u32;
    let stream_low = stream as u32;

    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();
    let format = g.u32(stream_low.wrapping_add(STREAM_FORMAT))?; // lwz r10,40(r4)
    let mut cutoff = normalised_cutoff(g, object_low, format)?;
    let ceiling = fp::load_single(g, CUTOFF_CEILING)?; // lfs f0,2176(r11)

    // blt cr6,0x82b27ec4 — at or above the ceiling (and on a NaN, which is unordered) the stage is
    // bypassed.
    if !(cutoff < ceiling) {
        let cached = fp::load_single(g, object_low.wrapping_add(CACHED_CUTOFF))?; // lfs f13,204(r3)
        // bge cr6,0x82b27eb0
        if cached < ceiling {
            let channels = g.u8(object_low.wrapping_add(CHANNEL_COUNT))? as u32; // lbz r11,42(r3)
            clear_history(g, &mut fpscr, object_low, channels)?;
        }
        // loc_82B27EB0
        fpscr.disable_flush_mode_unconditional();
        fp::store_single(g, object_low.wrapping_add(CACHED_CUTOFF), cutoff)?; // stfs f31,204(r30)
        return Ok(1); // li r3,1
    }

    // loc_82B27EC4
    fpscr.disable_flush_mode_unconditional();
    let floor_value = fp::load_single(g, CUTOFF_FLOOR)?; // lfs f0,2172(r11)
    // bge cr6,0x82b27ed4 — clamp up to the floor only when strictly below it.
    if cutoff < floor_value {
        cutoff = floor_value; // fmr f31,f0
    }

    // loc_82B27ED4
    fpscr.disable_flush_mode_unconditional();
    let cached = fp::load_single(g, object_low.wrapping_add(CACHED_CUTOFF))?; // lfs f0,204(r30)
    // beq cr6,0x82b27ef0 — an unchanged cutoff reuses the coefficients already at +184, and so never
    // reaches the trigonometry. That is the majority path and it needs no `Trig`.
    if cutoff != cached {
        // addi r3,r30,184 ; fmr f1,f31 ; bl 0x82b43cc0
        build_lowpass_coefficients(g, trig, object.wrapping_add(COEFFICIENTS as u64) as u32, cutoff)?;
        fpscr.disable_flush_mode_unconditional();
        fp::store_single(g, object_low.wrapping_add(CACHED_CUTOFF), cutoff)?; // stfs f31,204(r30)
    }

    // loc_82B27EF0
    filter_channels_and_swap(g, object, stream_low)?;
    Ok(1) // li r3,1
}

// ============================================================== sub_82B26568: the high-pass

/// `sub_82B26568` — run the per-channel high-pass over a 256-frame block, or bypass it.
///
/// The mirror of [`lowpass_stage`]: same arguments, same return of `1`, same write set, and the same
/// `Trig` cost on the recompute path. What differs is in the module's comparison table — the bypass
/// is at the **floor**, the clamp is **down** to the ceiling, the coefficients are built **inline**
/// rather than through `sub_82B43CC0`, and the numerator is `1 + cos` with `b1` **negated**.
///
/// **Writes** and reads as [`lowpass_stage`] does, with one difference in each direction: the four
/// coefficient cells are read by this body itself, so the C++ `Windows()` **does** declare them, and
/// the only read it leaves undeclared is the format pointer at `stream + 40`. The store order for the
/// five coefficients is `184, 188, 192, 200, 196` — not ascending — followed by the cache at `+204`.
///
/// One instruction is deliberately absent: `fmr f30,f31` between the two math calls. `f30` is never
/// read again and the epilogue reloads it, so the C++ port leaves the register at its entry value and
/// there is nothing here for it to be.
pub fn highpass_stage<T: Trig>(
    g: &mut Guest,
    trig: &mut T,
    object: u64,
    stream: u64,
) -> Result<u64> {
    let object_low = object as u32;
    let stream_low = stream as u32;

    let mut fpscr = Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();
    let format = g.u32(stream_low.wrapping_add(STREAM_FORMAT))?; // lwz r10,40(r4)
    let mut cutoff = normalised_cutoff(g, object_low, format)?;
    let floor_value = fp::load_single(g, CUTOFF_FLOOR)?; // lfs f0,2172(r11)

    // bgt cr6,0x82b26608 — at or below the floor (and on a NaN) the stage is bypassed and the pair
    // is not swapped. The mirror of the low-pass, which bypasses at the ceiling instead.
    if !(cutoff > floor_value) {
        let cached = fp::load_single(g, object_low.wrapping_add(CACHED_CUTOFF))?; // lfs f13,204(r3)
        // ble cr6,0x82b26600
        if cached > floor_value {
            let channels = g.u8(object_low.wrapping_add(CHANNEL_COUNT))? as u32; // lbz r11,42(r3)
            clear_history(g, &mut fpscr, object_low, channels)?;
        }
        // loc_82B26600, then straight to loc_82B26724: no kernel, no descriptor swap.
        fpscr.disable_flush_mode_unconditional();
        fp::store_single(g, object_low.wrapping_add(CACHED_CUTOFF), cutoff)?; // stfs f31,204(r30)
        return Ok(1); // li r3,1
    }

    // loc_82B26608
    fpscr.disable_flush_mode_unconditional();
    let ceiling = fp::load_single(g, CUTOFF_CEILING)?; // lfs f0,2176(r11)
    // ble cr6,0x82b26618 — clamp down to the ceiling only when strictly above it.
    if cutoff > ceiling {
        cutoff = ceiling; // fmr f31,f0
    }

    // loc_82B26618
    fpscr.disable_flush_mode_unconditional();
    let cached = fp::load_single(g, object_low.wrapping_add(CACHED_CUTOFF))?; // lfs f0,204(r30)
    // beq cr6,0x82b266ac
    if cutoff != cached {
        let sine_raw = trig.sine(g, cutoff)?; // fmr f1,f31 ; bl 0x82f4ded0
        fpscr.disable_flush_mode_unconditional();
        let sine = fp::frsp(sine_raw); // frsp f29,f1
        let cosine_raw = trig.cosine(g, cutoff)?; // fmr f1,f31 ; bl 0x82f4dfb0
        fpscr.disable_flush_mode_unconditional();
        let cosine = fp::frsp(cosine_raw); // frsp f11,f1

        let alpha = fp::mul_single(sine, fp::load_single(g, HALF_SINGLE)?); // fmuls f10,f29,f0
        let one = fp::load_single(g, ONE_SINGLE)?; // lfs f0,-22460(r10)
        // fadds f9,f11,f0 — the high-pass form, `1 + cos`. sub_82B43CC0 does an fsubs here.
        let numerator = fp::add_single(cosine, one);
        // fmuls f8,f11,f13
        let neg_two_cos = fp::mul_single(cosine, fp::load_single(g, MINUS_TWO_SINGLE)?);
        let a0 = fp::add_single(alpha, one); // fadds f7,f10,f0
        let one_minus_alpha = fp::sub_single(one, alpha); // fsubs f6,f0,f10
        let inv_a0 = fp::div_single(one, a0); // fdivs f5,f0,f7
        let two_a0 = fp::mul_single(a0, fp::load_single(g, TWO_SINGLE)?); // fmuls f4,f7,f12
        let half_num = fp::mul_single(numerator, inv_a0); // fmuls f3,f9,f5

        // Store order 184, 188, 192, 200, 196, 204 — kept as the original has it.
        let at = |k: u32| object_low.wrapping_add(COEFFICIENTS).wrapping_add(k);
        fp::store_single(g, at(dsp::biquad::COEFF_A1), fp::mul_single(neg_two_cos, inv_a0))?;
        fp::store_single(g, at(dsp::biquad::COEFF_A2), fp::mul_single(one_minus_alpha, inv_a0))?;
        fp::store_single(g, at(dsp::biquad::COEFF_B0), fp::div_single(numerator, two_a0))?;
        fp::store_single(g, at(dsp::biquad::COEFF_B2), fp::div_single(numerator, two_a0))?;
        // fneg f12,f3 — a sign-bit flip on the doubleword, not an arithmetic negate. This single
        // instruction is what makes the stage a high-pass rather than a low-pass.
        fp::store_single(g, at(dsp::biquad::COEFF_B1), fp::neg_double(half_num))?;
        fp::store_single(g, object_low.wrapping_add(CACHED_CUTOFF), cutoff)?; // stfs f31,204(r30)
    }

    // loc_82B266AC
    filter_channels_and_swap(g, object, stream_low)?;
    Ok(1) // loc_82B26724: li r3,1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mathlib::{Unported, tests::Scripted};

    const BASE: u32 = 0x4000_0000;
    const OBJECT: u32 = BASE;
    // Everything else sits above `OBJECT + 1024`, so a test can aim a 1 KB channel write at the
    // object itself without also clobbering the stream it is reading through.
    const STREAM: u32 = BASE + 0x800;
    const FORMAT: u32 = BASE + 0x840;
    const DESC_A: u32 = BASE + 0x880;
    const DESC_B: u32 = BASE + 0x8C0;
    const BUF_A: u32 = BASE + 0x1000;
    const BUF_B: u32 = BASE + 0x3000;
    const COEFFS: u32 = BASE + 0x8000;

    /// A guest with the object, the stream, two descriptors, two 8-channel buffers, and every rodata
    /// cell the three bodies read — each as its **own** four-byte segment, so a body that computed
    /// the wrong address gets an `Err` out of [`Guest`] rather than a plausible neighbouring float.
    fn guest() -> Guest {
        let mut g = Guest::single(BASE, 0x9000);
        for (cell, value) in [
            (CUTOFF_SCALE, 1.0f32),
            (CUTOFF_FLOOR, 0.01),
            (CUTOFF_CEILING, 1.0),
            (ZERO_SINGLE, 0.0),
            (KERNEL_BIAS, 0.0),
            (HALF_SINGLE, 0.5),
            (ONE_SINGLE, 1.0),
            (MINUS_TWO_SINGLE, -2.0),
            (TWO_SINGLE, 2.0),
        ] {
            g.put(cell, value.to_bits().to_be_bytes().to_vec());
        }
        g.set_u32(STREAM + STREAM_FORMAT, FORMAT).unwrap();
        g.set_u32(STREAM + STREAM_BUFFER_A, DESC_A).unwrap();
        g.set_u32(STREAM + STREAM_BUFFER_B, DESC_B).unwrap();
        g.set_u32(DESC_A + DESC_BASE, BUF_A).unwrap();
        g.set_u16(DESC_A + DESC_STRIDE, 256).unwrap();
        g.set_u32(DESC_B + DESC_BASE, BUF_B).unwrap();
        g.set_u16(DESC_B + DESC_STRIDE, 256).unwrap();
        g
    }

    /// `numerator / rate * scale` with rate and scale both 1.0, so the cutoff *is* the `+52` word.
    fn cutoff(g: &mut Guest, value: f32) {
        g.set_u32(FORMAT + FORMAT_SAMPLE_RATE, 1.0f32.to_bits()).unwrap();
        g.set_u32(OBJECT + CUTOFF_INPUT, value.to_bits()).unwrap();
    }

    fn cached(g: &mut Guest, value: f32) {
        g.set_u32(OBJECT + CACHED_CUTOFF, value.to_bits()).unwrap();
    }

    /// A unit-gain pass-through so the stage's output equals its input: `b0 = 1`, everything else 0.
    fn passthrough_coefficients(g: &mut Guest) {
        for k in 0..5u32 {
            g.set_u32(OBJECT + COEFFICIENTS + 4 * k, 0f32.to_bits()).unwrap();
        }
        g.set_u32(OBJECT + COEFFICIENTS + dsp::biquad::COEFF_B0, 1.0f32.to_bits()).unwrap();
    }

    fn channels(g: &mut Guest, n: u8) {
        g.set_u8(OBJECT + CHANNEL_COUNT, n).unwrap();
    }

    /// A distinct ramp in each of `n` channels of `BUF_A`, and a poison in `BUF_B`.
    fn input(g: &mut Guest, n: u32) {
        for c in 0..n {
            for i in 0..BLOCK_FRAMES {
                let v = c as f32 * 1000.0 + i as f32;
                g.set_u32(BUF_A + 1024 * c + 4 * i, v.to_bits()).unwrap();
                g.set_u32(BUF_B + 1024 * c + 4 * i, 0x7F7F_7F7F).unwrap();
            }
        }
    }

    fn state_words(g: &Guest, channel: u32) -> Vec<u32> {
        (0..4).map(|k| g.u32(OBJECT + FILTER_STATE + 16 * channel + 4 * k).unwrap()).collect()
    }

    fn poison_state(g: &mut Guest, n: u32) {
        for c in 0..n {
            for k in 0..4u32 {
                g.set_u32(OBJECT + FILTER_STATE + 16 * c + 4 * k, 0xDEAD_0000 + k).unwrap();
            }
        }
    }

    fn coefficients_at(g: &Guest, at: u32) -> Vec<f32> {
        (0..5).map(|k| g.f32(at + 4 * k).unwrap()).collect()
    }

    // ------------------------------------------------------------------ sub_82B43CC0

    /// The five coefficients the RBJ low-pass at Q = 1 gives for this sine and cosine, computed in
    /// the lifted operation order and with the lifted roundings. Shares that order with the port by
    /// construction, so what it checks is which expression lands in which slot, not the rounding —
    /// `the_second_b2_divide_is_not_a_copy_of_b0` and the store-order test carry that half.
    fn model_lowpass(sine: f32, cosine: f32) -> [f32; 5] {
        model_lowpass_with_half(sine, cosine, 0.5)
    }

    fn model_lowpass_with_half(sine: f32, cosine: f32, half: f64) -> [f32; 5] {
        let (s, c) = (sine as f64, cosine as f64);
        let alpha = fp::mul_single(s, half);
        let numerator = fp::sub_single(1.0, c);
        let a0 = fp::add_single(alpha, 1.0);
        let inv = fp::div_single(1.0, a0);
        let two_a0 = fp::mul_single(a0, 2.0);
        [
            fp::mul_single(fp::mul_single(c, -2.0), inv) as f32,
            fp::mul_single(fp::sub_single(1.0, alpha), inv) as f32,
            fp::div_single(numerator, two_a0) as f32,
            fp::mul_single(numerator, inv) as f32,
            fp::div_single(numerator, two_a0) as f32,
        ]
    }

    #[test]
    fn the_coefficients_are_the_rbj_low_pass_at_unit_q() {
        let mut g = guest();
        let mut trig = Scripted { sine: 0.5, cosine: 0.75, ..Default::default() };
        build_lowpass_coefficients(&mut g, &mut trig, COEFFS, 0.25).unwrap();

        assert_eq!(coefficients_at(&g, COEFFS), model_lowpass(0.5, 0.75));
        // Both helpers were asked, both with the angle, sine first.
        assert_eq!(trig.asked, vec![('s', 0.25), ('c', 0.25)]);
        // b0 and b2 are the same expression; a1 and b1 are not, so the slots are not interchangeable.
        let c = coefficients_at(&g, COEFFS);
        assert_eq!(c[2], c[4], "b0 and b2 are the same quotient");
        assert_ne!(c[0], c[3]);
    }

    #[test]
    fn the_four_constants_are_read_live_from_the_map() {
        // Not folded in: patch `half` and alpha moves, which moves a0 and therefore four of the five.
        let mut g = guest();
        let mut trig = Scripted { sine: 1.0, cosine: 0.0, ..Default::default() };
        build_lowpass_coefficients(&mut g, &mut trig, COEFFS, 0.0).unwrap();
        let before = coefficients_at(&g, COEFFS);

        let mut patched = guest();
        patched.set_u32(HALF_SINGLE, 0.25f32.to_bits()).unwrap();
        let mut t2 = Scripted { sine: 1.0, cosine: 0.0, ..Default::default() };
        build_lowpass_coefficients(&mut patched, &mut t2, COEFFS, 0.0).unwrap();
        let after = coefficients_at(&patched, COEFFS);

        assert_ne!(before, after, "the 0.5 cell is loaded, not a literal");
        assert_eq!(before, model_lowpass_with_half(1.0, 0.0, 0.5));
        assert_eq!(after, model_lowpass_with_half(1.0, 0.0, 0.25), "alpha followed the cell");
    }

    #[test]
    fn each_constant_cell_is_read_on_every_call() {
        // One four-byte segment per cell, so dropping any one of the four is an `Err` rather than a
        // wrong number. That is the whole reason the test guest is built that way.
        for missing in [HALF_SINGLE, ONE_SINGLE, MINUS_TWO_SINGLE, TWO_SINGLE] {
            let mut g = guest();
            g.put(missing, Vec::new());
            let mut trig = Scripted { sine: 0.5, cosine: 0.5, ..Default::default() };
            assert!(
                build_lowpass_coefficients(&mut g, &mut trig, COEFFS, 0.0).is_err(),
                "cell {missing:#x} is not read"
            );
        }
    }

    #[test]
    fn an_unported_sine_refuses_and_writes_nothing() {
        let mut g = guest();
        for k in 0..5u32 {
            g.set_u32(COEFFS + 4 * k, 0x7F7F_7F7F).unwrap();
        }
        let e = build_lowpass_coefficients(&mut g, &mut Unported, COEFFS, 0.5).unwrap_err();
        assert_eq!(e.address, 0x82F4_DED0, "the sine, by guest address");
        for k in 0..5u32 {
            assert_eq!(g.u32(COEFFS + 4 * k).unwrap(), 0x7F7F_7F7F, "word {k} was written");
        }
    }

    #[test]
    fn the_cosines_double_tail_is_discarded_by_the_frsp() {
        // `frsp f11,f1` — a `Trig` that answers with more than 24 mantissa bits must give the same
        // coefficients as its own f32 rounding.
        //
        // The input is chosen so that the tail *survives* the next operation, which is the part a
        // weaker version of this test got wrong. `1 - 2^-25` is exactly half an f32 ulp below 1.0 and
        // rounds to 1.0; `fsubs(1.0, that)` is then `0.0` through the `frsp` and `2^-25` without it.
        // A value whose tail is lost in the following rounding anyway would not distinguish the two.
        //
        // **The sine's `frsp f30,f1` is invisible and this test does not claim otherwise.** Its only
        // consumer is `alpha = fmuls(sine, 0.5)`, and scaling by a power of two commutes with rounding
        // to single — so `(x·0.5)→f32` and `(x→f32)·0.5` agree for every non-denormal `x`. Measured
        // rather than argued: removing that one `frsp` leaves the whole suite green. It is reproduced
        // because the original has it, and it joins the module's "no test can catch this" list. The
        // same pair of facts holds for [`highpass_stage`]'s own two `frsp`s.
        let long = 1.0_f64 - f64::from_bits(0x3E60_0000_0000_0000); // 1 - 2^-25
        assert_ne!(long, long as f32 as f64, "the input has to distinguish the two");
        assert_eq!(long as f32, 1.0, "…and it has to round to something the next op treats apart");

        let mut g = guest();
        let mut trig = Scripted { sine: long, cosine: long, ..Default::default() };
        build_lowpass_coefficients(&mut g, &mut trig, COEFFS, 0.0).unwrap();

        let mut h = guest();
        let rounded = long as f32 as f64;
        let mut t2 = Scripted { sine: rounded, cosine: rounded, ..Default::default() };
        build_lowpass_coefficients(&mut h, &mut t2, COEFFS, 0.0).unwrap();

        assert_eq!(coefficients_at(&g, COEFFS), coefficients_at(&h, COEFFS));
        assert_eq!(coefficients_at(&g, COEFFS)[2], 0.0, "b0 came out of `1 - 1.0`, not `1 - long`");
    }

    // ------------------------------------------------------------------ the two stages

    #[test]
    fn the_low_pass_filters_and_swaps_when_the_cutoff_is_under_the_ceiling() {
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.5); // equal, so no trigonometry is reached at all
        passthrough_coefficients(&mut g);
        channels(&mut g, 2);
        input(&mut g, 3); // three channels of input and poison, two of which are in the count

        // `Unported` is deliberate: the majority path needs no sine, and a test that supplied one
        // would not establish that.
        assert_eq!(lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap(), 1);

        // b0 = 1 and everything else 0, so each destination channel is its source channel.
        for c in 0..2u32 {
            for i in [0u32, 1, 255] {
                let want = c as f32 * 1000.0 + i as f32;
                assert_eq!(g.f32(BUF_B + 1024 * c + 4 * i).unwrap(), want, "channel {c} frame {i}");
            }
        }
        // Channel 2 is past the count: its poison has to survive, and its source is non-zero so a
        // stray pass would be visible rather than writing the same zero.
        assert_eq!(g.u32(BUF_B + 2048).unwrap(), 0x7F7F_7F7F, "a third channel was filtered");
        // And the descriptor pair is swapped.
        assert_eq!(g.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_B);
        assert_eq!(g.u32(STREAM + STREAM_BUFFER_B).unwrap(), DESC_A);
    }

    #[test]
    fn the_low_pass_bypasses_at_the_ceiling_and_leaves_the_pair_alone() {
        // `!(cutoff < ceiling)`, so exactly 1.0 against a 1.0 ceiling bypasses: the test is strict.
        let mut g = guest();
        cutoff(&mut g, 1.0);
        cached(&mut g, 0.5); // below the ceiling, so the history is cleared
        channels(&mut g, 3);
        input(&mut g, 3);
        poison_state(&mut g, 4); // four poisoned slots, three in the count

        assert_eq!(lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap(), 1);

        assert_eq!(g.u32(BUF_B).unwrap(), 0x7F7F_7F7F, "the output block was written");
        assert_eq!(g.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_A, "the pair was swapped");
        assert_eq!(g.u32(STREAM + STREAM_BUFFER_B).unwrap(), DESC_B);
        for c in 0..3u32 {
            assert_eq!(state_words(&g, c), vec![0u32; 4], "channel {c}'s history survived");
        }
        assert_eq!(
            state_words(&g, 3),
            vec![0xDEAD_0000, 0xDEAD_0001, 0xDEAD_0002, 0xDEAD_0003],
            "a fourth slot, past the count, was cleared"
        );
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 1.0, "the cache is published either way");
    }

    #[test]
    fn the_bypass_clears_the_history_only_when_the_cache_was_itself_below_the_ceiling() {
        // `bge cr6,0x82b27eb0` — a cached cutoff at or above the ceiling means those words hold no
        // filter state, so they are left as they are. This is the branch a port most easily drops.
        let mut g = guest();
        cutoff(&mut g, 2.0);
        cached(&mut g, 1.0); // at the ceiling, not below it
        channels(&mut g, 1);
        poison_state(&mut g, 1);

        lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();

        assert_eq!(state_words(&g, 0), vec![0xDEAD_0000, 0xDEAD_0001, 0xDEAD_0002, 0xDEAD_0003]);
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 2.0);
    }

    #[test]
    fn the_history_clear_walks_four_words_per_channel_from_plus_fifty_six() {
        // The cursor starts at `+52` and the four stores are `+4`, `+8`, `+12` and then the advanced
        // cursor — so channel i's words are at `+56 + 16i` and the cutoff word at `+52` is **not**
        // one of them. A port that started the cursor at `+56` would clear `+60`…`+72` instead.
        let mut g = guest();
        cutoff(&mut g, 2.0);
        cached(&mut g, 0.5);
        channels(&mut g, 2);
        poison_state(&mut g, 3);

        lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();

        assert_eq!(state_words(&g, 0), vec![0u32; 4]);
        assert_eq!(state_words(&g, 1), vec![0u32; 4]);
        assert_eq!(
            state_words(&g, 2),
            vec![0xDEAD_0000, 0xDEAD_0001, 0xDEAD_0002, 0xDEAD_0003],
            "a third channel is past the count"
        );
        // `+52` is the cursor's *base*, and the first store is at `+4` off it — so the cutoff input
        // word survives the clear. A port that started the cursor at `+56` would clear `+60`…`+72`
        // instead, leaving channel 0's first word above untouched and the third channel's first word
        // cleared.
        assert_eq!(g.u32(OBJECT + CUTOFF_INPUT).unwrap(), 2.0f32.to_bits(), "the input word");
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 2.0);
    }

    #[test]
    fn the_low_pass_clamps_up_to_the_floor_and_the_clamp_is_strict() {
        // Below the floor the coefficients are built from the floor, not from the cutoff — and the
        // cache is published as the clamped value, which is how the next call knows not to rebuild.
        let mut g = guest();
        cutoff(&mut g, 0.001); // under a 0.01 floor
        cached(&mut g, 0.0);
        channels(&mut g, 0);
        let mut trig = Scripted { sine: 0.25, cosine: 0.5, ..Default::default() };

        lowpass_stage(&mut g, &mut trig, OBJECT as u64, STREAM as u64).unwrap();

        // The angle is the *widened* floor cell, not the decimal 0.01: `lfs` loads a single.
        let floor = 0.01f32 as f64;
        assert_eq!(trig.asked, vec![('s', floor), ('c', floor)], "the floor, not 0.001");
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 0.01);
        assert_eq!(coefficients_at(&g, OBJECT + COEFFICIENTS), model_lowpass(0.25, 0.5));

        // Exactly on the floor is not clamped, which is the same value here — so the test that the
        // compare is strict has to come from *above* the floor instead.
        let mut h = guest();
        cutoff(&mut h, 0.02);
        cached(&mut h, 0.0);
        channels(&mut h, 0);
        let mut t2 = Scripted { sine: 0.25, cosine: 0.5, ..Default::default() };
        lowpass_stage(&mut h, &mut t2, OBJECT as u64, STREAM as u64).unwrap();
        let kept = 0.02f32 as f64;
        assert_eq!(t2.asked, vec![('s', kept), ('c', kept)], "a cutoff above the floor is kept");
    }

    #[test]
    fn an_unchanged_cutoff_skips_the_rebuild_entirely() {
        // `beq cr6,0x82b27ef0`. With `Unported` this is observable as the difference between a
        // completed call and an `Err`, which is the sharpest form the distinction can take.
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.5);
        channels(&mut g, 0);
        assert!(lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).is_ok());

        let mut h = guest();
        cutoff(&mut h, 0.5);
        cached(&mut h, 0.5000001);
        channels(&mut h, 0);
        let e = lowpass_stage(&mut h, &mut Unported, OBJECT as u64, STREAM as u64).unwrap_err();
        assert_eq!(e.address, 0x82F4_DED0, "a moved cutoff reaches the sine");
    }

    #[test]
    fn a_nan_cutoff_bypasses_both_stages() {
        // Both compares are unordered, so `!(x < c)` and `!(x > f)` are both true for a NaN. The
        // whole point of writing them as negations rather than as `>=` and `<=`.
        for (name, run) in [
            ("low", 0u8),
            ("high", 1u8),
        ] {
            let mut g = guest();
            g.set_u32(FORMAT + FORMAT_SAMPLE_RATE, 1.0f32.to_bits()).unwrap();
            g.set_u32(OBJECT + CUTOFF_INPUT, f32::NAN.to_bits()).unwrap();
            cached(&mut g, 0.5);
            channels(&mut g, 1);
            input(&mut g, 1);

            if run == 0 {
                lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
            } else {
                highpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
            }
            assert_eq!(g.u32(BUF_B).unwrap(), 0x7F7F_7F7F, "{name}: filtered a NaN cutoff");
            assert_eq!(g.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_A, "{name}: swapped");
            assert!(g.f32(OBJECT + CACHED_CUTOFF).unwrap().is_nan(), "{name}: cache");
        }
    }

    #[test]
    fn the_high_pass_bypasses_at_the_floor_where_the_low_pass_filters() {
        // The two stages disagree about the same cutoff, which is the clearest statement that they
        // are mirrors: 0.005 is under the 0.01 floor, so the high-pass bypasses and the low-pass
        // clamps up and filters.
        let mut high = guest();
        cutoff(&mut high, 0.005);
        cached(&mut high, 0.0);
        channels(&mut high, 1);
        input(&mut high, 1);
        highpass_stage(&mut high, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(high.u32(BUF_B).unwrap(), 0x7F7F_7F7F, "the high-pass bypassed");
        assert_eq!(high.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_A);

        // And a cutoff *between* the two cells is what pins which of them the high-pass tests: 0.5
        // is above the floor and below the ceiling, so it filters. Reading the ceiling there instead
        // would bypass.
        let mut mid = guest();
        cutoff(&mut mid, 0.5);
        cached(&mut mid, 0.5); // equal, so no rebuild and no Trig
        passthrough_coefficients(&mut mid);
        channels(&mut mid, 1);
        input(&mut mid, 1);
        highpass_stage(&mut mid, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(mid.f32(BUF_B + 4).unwrap(), 1.0, "a mid-band cutoff filters");
        assert_eq!(mid.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_B);

        let mut low = guest();
        cutoff(&mut low, 0.005);
        cached(&mut low, 0.01); // the *clamped* value, so no rebuild and no Trig
        passthrough_coefficients(&mut low);
        channels(&mut low, 1);
        input(&mut low, 1);
        lowpass_stage(&mut low, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(low.f32(BUF_B + 4).unwrap(), 1.0, "the low-pass filtered");
        assert_eq!(low.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_B);
    }

    #[test]
    fn the_high_pass_clamps_down_to_the_ceiling() {
        let mut g = guest();
        cutoff(&mut g, 4.0); // above a 1.0 ceiling
        cached(&mut g, 0.0);
        channels(&mut g, 0);
        let mut trig = Scripted { sine: 0.25, cosine: 0.5, ..Default::default() };
        highpass_stage(&mut g, &mut trig, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(trig.asked, vec![('s', 1.0), ('c', 1.0)], "the ceiling, not 4.0");
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 1.0);
    }

    #[test]
    fn the_high_pass_coefficients_differ_from_the_low_pass_in_the_numerator_and_in_b1() {
        // One sine and cosine, two bodies, five slots. a1 and a2 must agree exactly; b0 and b2 must
        // use `1 + cos` where the low-pass uses `1 - cos`; and b1 must be the *negation* of
        // `num/a0`. Writing the expected values out by hand rather than through a second model is
        // what makes this a check on the port and not on a shared helper.
        let (s, c) = (0.25f32, 0.5f32);
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.0);
        channels(&mut g, 0);
        let mut trig = Scripted { sine: s as f64, cosine: c as f64, ..Default::default() };
        highpass_stage(&mut g, &mut trig, OBJECT as u64, STREAM as u64).unwrap();
        let high = coefficients_at(&g, OBJECT + COEFFICIENTS);

        let mut h = guest();
        let mut t2 = Scripted { sine: s as f64, cosine: c as f64, ..Default::default() };
        build_lowpass_coefficients(&mut h, &mut t2, COEFFS, 0.5).unwrap();
        let low = coefficients_at(&h, COEFFS);

        assert_eq!(high[0], low[0], "a1 = -2cos/a0 in both");
        assert_eq!(high[1], low[1], "a2 = (1-alpha)/a0 in both");

        let alpha = fp::mul_single(s as f64, 0.5);
        let a0 = fp::add_single(alpha, 1.0);
        let inv = fp::div_single(1.0, a0);
        let two_a0 = fp::mul_single(a0, 2.0);
        let numerator = fp::add_single(c as f64, 1.0); // 1 + cos
        assert_eq!(high[2], fp::div_single(numerator, two_a0) as f32, "b0 = (1+cos)/(2 a0)");
        assert_eq!(high[4], high[2], "b2 is the same quotient, computed twice");
        assert_eq!(high[3], fp::neg_double(fp::mul_single(numerator, inv)) as f32, "b1 is negated");
        assert!(high[3] < 0.0 && low[3] > 0.0, "and the sign is the difference from the low-pass");
    }

    #[test]
    fn the_channel_count_is_reloaded_at_the_bottom_of_the_filter_loop() {
        // `lbz r11,42(r30)` after every kernel call. The object is arranged so that channel 0's
        // *destination* block covers `+42`, and the byte written there is a zero — so a hoisted
        // count would filter all four channels and the reloaded one stops after the first.
        //
        // The destination buffer is the object itself here, which is exactly the aliasing layout the
        // C++ `Windows()` declines. So what this establishes is that the reload survived the
        // transcription, not that the guest agrees with the answer.
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.5);
        passthrough_coefficients(&mut g);
        channels(&mut g, 4);
        input(&mut g, 4);
        // Point the destination at the object, so filtering channel 0 writes 1 KB from +0 — which
        // includes +42. Frame 10 of the ramp is 10.0f32 = 0x41200000, whose byte at +42 is zero.
        g.set_u32(DESC_B + DESC_BASE, OBJECT).unwrap();
        g.set_u32(OBJECT + 1024, 0x7F7F_7F7F).unwrap(); // channel 1's destination, if it ran

        lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();

        assert_eq!(g.u8(OBJECT + CHANNEL_COUNT).unwrap(), 0, "the pass cleared its own count");
        // Channel 1's destination sits past the 1 KB channel 0 covered, and its source is 1000.0.
        assert_eq!(g.u32(OBJECT + 1024).unwrap(), 0x7F7F_7F7F, "the loop ran a second channel");
    }

    #[test]
    fn a_zero_channel_count_still_swaps_the_pair_and_rebuilds() {
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.0);
        channels(&mut g, 0);
        let mut trig = Scripted { sine: 0.25, cosine: 0.5, ..Default::default() };
        lowpass_stage(&mut g, &mut trig, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(trig.asked.len(), 2, "the rebuild happened");
        assert_eq!(g.u32(STREAM + STREAM_BUFFER_A).unwrap(), DESC_B, "and the pair swapped");
    }

    #[test]
    fn the_state_pointer_advances_sixteen_bytes_per_channel() {
        // Every channel must carry its own four history words. With a coefficient set whose output
        // depends on the history, two channels filtered from *identical* input have to produce
        // identical output — which they only do if each read its own slot rather than sharing one.
        let mut g = guest();
        cutoff(&mut g, 0.5);
        cached(&mut g, 0.5);
        channels(&mut g, 2);
        // b0 = 1, a1 = -1: y[k] = x[k] + y[k-1], a running sum that is extremely sensitive to the
        // carried y1.
        for k in 0..5u32 {
            g.set_u32(OBJECT + COEFFICIENTS + 4 * k, 0f32.to_bits()).unwrap();
        }
        g.set_u32(OBJECT + COEFFICIENTS + dsp::biquad::COEFF_B0, 1.0f32.to_bits()).unwrap();
        g.set_u32(OBJECT + COEFFICIENTS + dsp::biquad::COEFF_A1, (-1.0f32).to_bits()).unwrap();
        for c in 0..2u32 {
            for i in 0..BLOCK_FRAMES {
                g.set_u32(BUF_A + 1024 * c + 4 * i, 1.0f32.to_bits()).unwrap();
            }
        }
        // Channel 0's history is seeded; channel 1's is left at zero.
        g.set_u32(OBJECT + FILTER_STATE + dsp::biquad::HISTORY_Y1, 100.0f32.to_bits()).unwrap();

        lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();

        assert_eq!(g.f32(BUF_B).unwrap(), 101.0, "channel 0 saw its own seeded y1");
        assert_eq!(g.f32(BUF_B + 1024).unwrap(), 1.0, "channel 1 saw its own zero");
        // And each slot carries its own y[255] back out.
        assert_ne!(state_words(&g, 0), state_words(&g, 1));
    }

    #[test]
    fn the_format_pointer_is_dereferenced_before_anything_is_written() {
        // `lwz r10,40(r4)` then `lfs f13,12(r10)`. A null or unmapped format block is an `Err` here
        // and the original would load through it; the C++ `Windows()` declines the call for exactly
        // that reason, so nothing is known about what the guest does either.
        let mut g = guest();
        g.set_u32(STREAM + STREAM_FORMAT, 0).unwrap();
        cached(&mut g, 9.0);
        channels(&mut g, 1);
        assert!(lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).is_err());
        assert!(highpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).is_err());
        assert_eq!(g.f32(OBJECT + CACHED_CUTOFF).unwrap(), 9.0, "a refused call wrote the cache");
    }

    #[test]
    fn the_cutoff_is_two_roundings_and_not_one_fma() {
        // `fdivs` then `fmuls`: the quotient is narrowed to a single before it is scaled. An input
        // where the two forms disagree pins it.
        let (n, r, s) = (1.0f32, 3.0f32, 7.0f32);
        let two = fp::mul_single(fp::div_single(n as f64, r as f64), s as f64);
        let one = fp::mul_single(n as f64 / r as f64, s as f64);
        assert_ne!(two, one, "the distinguishing input has to distinguish");

        let mut g = guest();
        g.set_u32(CUTOFF_SCALE, s.to_bits()).unwrap();
        g.set_u32(CUTOFF_CEILING, 1e9f32.to_bits()).unwrap();
        g.set_u32(FORMAT + FORMAT_SAMPLE_RATE, r.to_bits()).unwrap();
        g.set_u32(OBJECT + CUTOFF_INPUT, n.to_bits()).unwrap();
        cached(&mut g, 0.0);
        channels(&mut g, 0);
        let mut trig = Scripted { sine: 0.0, cosine: 0.0, ..Default::default() };
        lowpass_stage(&mut g, &mut trig, OBJECT as u64, STREAM as u64).unwrap();
        assert_eq!(trig.asked, vec![('s', two), ('c', two)]);
    }

    #[test]
    fn both_stages_restore_the_entry_flush_mode() {
        for high in [false, true] {
            let mut g = guest();
            cutoff(&mut g, 0.5);
            cached(&mut g, 0.5);
            passthrough_coefficients(&mut g);
            channels(&mut g, 1);
            input(&mut g, 1);
            let before = crate::vmx::get_mxcsr();
            if high {
                highpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
            } else {
                lowpass_stage(&mut g, &mut Unported, OBJECT as u64, STREAM as u64).unwrap();
            }
            assert_eq!(crate::vmx::get_mxcsr(), before);
        }
    }
}
