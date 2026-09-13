//! The gain plumbing: the two bodies that drive the DSP kernels over a channel descriptor.
//!
//! Both take a descriptor — `+4` channel 0's float array, `+14` a `u16` count of singles between one
//! channel and the next — walk its channels, and hand each one to a kernel that
//! [`crate::dsp`] already has. They are the layer between [`crate::spatial`], which decides *what*
//! gain a speaker gets, and the kernels, which apply it to samples.
//!
//! | function | guest | `docs/ports.md` | lifted lines | calls/boot | calls/play | kernel |
//! |---|---|---|---|---|---|---|
//! | [`apply_gain_matrix`] | `sub_82B29AF0` | verified | 143 | 245,762 | 404,782 | [`crate::dsp::scale`] |
//! | [`ramp_channels`] | `sub_82B23B50` | verified | 119 | 292,165 | 435,555 | [`crate::dsp::gain_ramp`] |
//!
//! Both `.inc` headers lead with `// STATUS: verified` and `docs/ports.md` agrees. They are here
//! rather than in `dsp/` because neither is a kernel: they contain no sample arithmetic at all, only
//! loop bounds, gain-cursor arithmetic and channel addresses.
//!
//! **Unit-tested against a verified reference**, the crate README's second kind of green. The C++
//! bodies were compared call-for-call against the original under the shadow harness on real inputs
//! at zero divergence; the Rust has no recorded vectors of its own.
//!
//! ## The three arithmetic rules these two exist to get right
//!
//! Neither function does anything interesting with floats — one `lfsu` per gain, one `fsubs` and one
//! `fmuls` for a ramp step. What they do is **compute addresses**, and `CLAUDE.md` names all three
//! traps:
//!
//! 1. `mullw` is a **64-bit** product of two sign-extended words, so `stride * channel` can exceed
//!    32 bits before anything truncates it;
//! 2. `rlwinm rX,rX,2,0,29` is a 32-bit shift left by two of the product's **low word** — the
//!    rotate's wrapped bits are masked off;
//! 3. `add` is 64-bit on two zero-extended words, so a channel address can **carry into bit 32**,
//!    and only the callee's own address truncation drops it. Keeping the chain in 64 bits and
//!    casting at the call is the rule that surfaced on the 120th call of another function.
//!
//! [`channel_address`] is that chain, written once and used by both. `crate::mix`'s `row_offset` is
//! the same `mullw`/`rlwinm` pair with the sum taken in 32 bits, because its caller adds in 32 bits;
//! the two are deliberately not shared.
//!
//! ## Reloads reproduced rather than hoisted
//!
//! Every loop bound and every descriptor field is re-read from guest memory on each pass:
//! `sub_82B29AF0` reloads the destination count after **every** kernel call and both descriptors'
//! base and stride on every iteration; `sub_82B23B50` re-reads the applied gain, both strides and
//! both buffers per channel. Both C++ `Windows()` builders decline a call whose output covers any of
//! those words, so a caller whose buffers overlap its own control block is input the harness has
//! never compared. The reloads are kept because the originals have them; what that establishes is
//! that they survived the transcription, not that the guest agrees on such a layout.
//!
//! One reload is load-bearing rather than defensive, and it is worth reading twice.
//! `sub_82B29AF0`'s destination count lives in **one** variable across both passes: the second pass
//! tests whatever the *last* reload of the first pass left there, not a fresh read. If the count was
//! zero at entry the first pass never runs, the variable is still the entry value, and the inner loop
//! stays skipped for every source. Modelling it as two variables would be wrong.
//!
//! ## What no test here can catch
//!
//! Four of these reproductions are invisible to every test in the crate. Each was measured — the
//! change made, all 275 tests re-run, all still green — not argued:
//!
//! - **The `int32_t` casts in [`channel_offset`]'s `mullw`.** Zero-extending both operands instead
//!   gives the identical answer *always*, not merely on these inputs: only the product's low word
//!   survives the `rlwinm`, and a signed and an unsigned multiply agree on every low word. So the
//!   64-bitness of `mullw` cannot be observed through this function at all. It is written signed
//!   because the instruction is, and the rule it protects lives one line down, in the **add**, which
//!   `a_channel_address_is_a_64_bit_sum_of_a_32_bit_offset` does pin.
//! - **[`ramp_channels`] re-reads the applied gain on every iteration.** Nothing inside the loop
//!   writes that word, so hoisting the load is invisible. What is *not* invisible is moving the
//!   tail's latch into the loop, which `every_channel_ramps_from_the_same_starting_gain` catches —
//!   that is the mistranscription this reload actually guards against.
//! - **[`apply_gain_matrix`] carries one destination count across both passes.** The C++ note is
//!   emphatic that "modelling that as two variables would be wrong", and it is right about the
//!   transcription — but replacing pass 2's test with a fresh read leaves the suite green, and the
//!   reason is stronger than "these inputs do not reach it": pass 1's last act before exiting is to
//!   reload that word, and nothing writes it between then and pass 2's test, so the carried value
//!   and a fresh read **cannot** differ on any input. Kept as the original has it.
//! - **[`apply_gain_matrix`] re-reads both descriptor fields on every iteration.** Hoisting them out
//!   of the inner loop is invisible, for the same reason the C++ `Windows()` declines a call whose
//!   output covers them: no comparable input makes them change mid-call.
//!
//! ## What is not reproduced
//!
//! `sub_82B29AF0`'s `stwu r1,-160(r1)`. The C++ **does** reproduce it, because both kernels spill
//! `f1` and their callee-saved registers below the live `r1` and read that scratch back. The Rust
//! kernels keep those values in registers instead — `crate::dsp::scale`'s own note says so — so
//! nothing in this chain writes the guest stack and there is nothing to make room for. Same position
//! `crate::dsp::biquad` is in with its own frame.

use crate::dsp::gain_ramp::{self, GainRampClobbers};
use crate::dsp::scale;
use crate::vmx::Fpscr;
use crate::{Guest, Result, fp};

// ------------------------------------------------------------------------- the shared descriptor

/// `lwz r10,4(r30)` — channel 0's float array. The same offset [`crate::mix::ROW_BASE`] names on the
/// mixer-side object, which is the same structure seen from another caller.
pub const BUFFER_BASE: u32 = 4;
/// `lhz r11,14(r30)` — `u16` singles between one channel and the next. Not bytes: it is scaled by
/// four. Matches [`crate::mix::ROW_STRIDE`].
pub const CHANNEL_STRIDE: u32 = 14;
/// 16 — the span both C++ `Windows()` builders alias-check, so the whole descriptor.
pub const DESCRIPTOR_BYTES: u32 = 16;

const _: () = assert!(BUFFER_BASE == crate::mix::ROW_BASE, "the same descriptor mix.rs walks");
const _: () = assert!(CHANNEL_STRIDE == crate::mix::ROW_STRIDE);

/// `rlwinm rX,rX,2,0,29` applied to `mullw stride,index`: the byte offset of one channel.
///
/// Two rules in three lines. The product is 64-bit on **sign-extended** words; only its low word is
/// then shifted left by two, and the mask is what makes that a shift rather than a rotate.
pub fn channel_offset(stride: u32, index: u32) -> u32 {
    let elements = (stride as i32 as i64).wrapping_mul(index as i32 as i64); // mullw
    ((elements as u64 as u32) << 2) & 0xFFFF_FFFC // rlwinm rX,rX,2,0,29
}

/// `add rD,rOffset,rBuffer` on two zero-extended words: **64-bit**, so the sum can carry into bit
/// 32.
///
/// Returned as a `u64` on purpose. Every caller here passes it to a kernel whose `r3` is a full
/// 64-bit register, and the truncation to a guest address happens inside that kernel. A chain
/// truncated to 32 bits leaves memory byte-identical and the register wrong, which is exactly the
/// failure `CLAUDE.md` records surfacing on the 120th call of one function.
pub fn channel_address(stride: u32, index: u32, buffer: u32) -> u64 {
    u64::from(channel_offset(stride, index)) + u64::from(buffer)
}

// ================================================================ sub_82B29AF0: the gain matrix

/// `f32[source][dest]` — row `s` at `+444 + 32*s`, column `d` at `+4*d`.
///
/// `addi r29,r3,440` with `lfsu`'s `+4` pre-increment is what puts `gain[0][0]` at `+444`, and
/// [`GAIN_CURSOR_BASE`] is the 440 the port carries so the pre-increment is not folded away.
pub const GAIN_MATRIX: u32 = 444;
/// The cursor the `lfsu` chain starts from: `GAIN_MATRIX - 4`.
pub const GAIN_CURSOR_BASE: u32 = GAIN_MATRIX - 4;
/// `addi r24,r24,32` — eight floats per source row, so at most eight destinations are addressable.
pub const GAIN_ROW_STRIDE: u32 = 32;
/// `u32` source channel count. Only a value above 1 reaches the accumulating pass.
pub const SOURCE_COUNT: u32 = 748;
/// `u32` destination channel count. **Reloaded after every kernel call**, and carried across both
/// passes in one variable — see the module note.
pub const DEST_COUNT: u32 = 752;
/// `li r6,256` at both call sites. Not a parameter: the kernels are called with a literal count.
pub const SAMPLES: u32 = 256;

const _: () = assert!(GAIN_CURSOR_BASE == 440, "addi r29,r3,440");
const _: () = assert!(GAIN_MATRIX + GAIN_ROW_STRIDE == 476, "addi r24,r28,476 for row 1");

/// `sub_82B29AF0` — apply the channel gain matrix.
///
/// `mixer` is `r3`, `dst_desc` is `r4` (the **destination** descriptor) and `src_desc` is `r5` (the
/// **source** one). No result (`kReturnNone`). The direction is not a guess: the kernels take
/// `r3 = dst`, and this function builds their `r3` out of `r4`'s fields.
///
/// Two passes, and the split is the whole point:
///
/// - pass 1 walks destinations `d` and calls [`crate::dsp::scale::scale`] — `dst[k] = src[k]·gain` —
///   with source channel 0, so it **overwrites**;
/// - pass 2 walks sources `s = 1..` and destinations `d` again and calls
///   [`crate::dsp::scale::scale_accumulate`] — `dst[k] += src[k]·gain`.
///
/// Overwrite-then-accumulate is why source 0 gets a different kernel, and why a matrix mix needs no
/// separate clear pass.
///
/// **Writes** exactly the destination channels, 1024 bytes each: both kernels write `[r3, r3 + 4·256)`
/// and nothing else. Pass 2 revisits the same destinations, so one sweep names the whole write set.
/// **Reads** the two counts, the gain matrix, both descriptors, the source channels, and — because
/// the accumulating kernel reads its accumulator — the destination channels too.
///
/// The gains come off one `lfsu` cursor per pass, never an indexed load: pass 1 walks `+444` upward
/// through row 0, and pass 2 restarts the cursor at `row - 4` for each source. A port that indexed
/// the matrix instead would agree on every well-formed mixer and differ the moment a count changed
/// mid-call, which is precisely what the reload makes possible.
pub fn apply_gain_matrix(g: &mut Guest, mixer: u32, dst_desc: u32, src_desc: u32) -> Result<()> {
    let mut fpscr = Fpscr::capture();

    // r11 — one variable across both passes. See the module note; this is not two reads.
    let mut dest_count = g.u32(mixer + DEST_COUNT)?; // lwz r11,752(r3)
    let mut source = u64::from(g.u32(src_desc + BUFFER_BASE)?); // lwz r27,4(r5)

    // cmplwi cr6,r11,0 ; beq cr6 — pass 1: source channel 0 over every destination.
    if dest_count != 0 {
        let mut gain = mixer.wrapping_add(GAIN_CURSOR_BASE); // addi r29,r3,440
        let mut dest: u32 = 0; // li r31,0
        loop {
            // loc_82B29B20. Both descriptor fields are re-read every iteration.
            let stride = u32::from(g.u16(dst_desc + CHANNEL_STRIDE)?); // lhz r11,14(r30)
            let buffer = g.u32(dst_desc + BUFFER_BASE)?; // lwz r10,4(r30)
            fpscr.disable_flush_mode_unconditional(); // emitted at the lfsu
            gain = gain.wrapping_add(4); // lfsu f1,4(r29) updates the address first
            let k = fp::load_single(g, gain)?;
            let destination = channel_address(stride, dest, buffer);
            // bl 0x82b3bed8 — dst[i] = src[i]·k over 256 floats. The guest also hands the kernel
            // whatever the previous call left in r5; both kernels write r5 before reading it, so
            // that chain is invisible here and the Rust signature has no slot for it.
            scale::scale(g, destination as u32, source as u32, SAMPLES, k)?;
            dest_count = g.u32(mixer + DEST_COUNT)?; // lwz r11,752(r28) — after the call
            dest = dest.wrapping_add(1); // addi r31,r31,1
            if dest >= dest_count {
                break; // cmplw cr6,r31,r11 ; blt cr6
            }
        }
    }

    // loc_82B29B54.
    let mut source_count = g.u32(mixer + SOURCE_COUNT)?; // lwz r10,748(r28)
    // cmplwi cr6,r10,1 ; ble cr6 — one source channel means pass 1 was all of it.
    if source_count > 1 {
        let mut row = mixer.wrapping_add(GAIN_MATRIX + GAIN_ROW_STRIDE); // addi r24,r28,476
        let mut src: u32 = 1; // li r26,1
        loop {
            // loc_82B29B68.
            let stride = u32::from(g.u16(src_desc + CHANNEL_STRIDE)?); // lhz r10,14(r25)
            let buffer = g.u32(src_desc + BUFFER_BASE)?; // lwz r9,4(r25)
            source = channel_address(stride, src, buffer); // add r27,r10,r9
            // cmplwi cr6,r11,0 ; beq cr6 — the *carried* count, not a fresh read.
            if dest_count != 0 {
                let mut gain = row.wrapping_sub(4); // addi r29,r24,-4
                let mut dest: u32 = 0; // li r31,0
                loop {
                    // loc_82B29B8C.
                    let dst_stride = u32::from(g.u16(dst_desc + CHANNEL_STRIDE)?); // lhz r11,14(r30)
                    let dst_buffer = g.u32(dst_desc + BUFFER_BASE)?; // lwz r10,4(r30)
                    fpscr.disable_flush_mode_unconditional();
                    gain = gain.wrapping_add(4); // lfsu f1,4(r29)
                    let k = fp::load_single(g, gain)?;
                    let destination = channel_address(dst_stride, dest, dst_buffer);
                    // bl 0x82b44b20 — dst[i] += src[i]·k over 256 floats.
                    scale::scale_accumulate(g, destination as u32, source as u32, SAMPLES, k)?;
                    dest_count = g.u32(mixer + DEST_COUNT)?; // lwz r11,752(r28)
                    dest = dest.wrapping_add(1);
                    if dest >= dest_count {
                        break;
                    }
                }
            }
            // loc_82B29BC0.
            source_count = g.u32(mixer + SOURCE_COUNT)?; // lwz r10,748(r28)
            src = src.wrapping_add(1); // addi r26,r26,1
            row = row.wrapping_add(GAIN_ROW_STRIDE); // addi r24,r24,32
            if src >= source_count {
                break; // cmplw cr6,r26,r10 ; blt cr6
            }
        }
    }
    Ok(())
}

// ============================================================== sub_82B23B50: the per-channel ramp

/// `lbz r27,42(r30)` — `u8` channel count, the loop trip count. The same offset
/// [`crate::mix::CHANNELS`] names.
pub const CHANNEL_COUNT: u32 = 42;
/// `lfs f0,52(r3)` — the target gain, latched into [`APPLIED_GAIN`] on the way out.
pub const TARGET_GAIN: u32 = 52;
/// `lfs f13,56(r30)` — the gain actually applied to the previous block. **Re-read every
/// iteration** and stored twice per call on the restart path.
pub const APPLIED_GAIN: u32 = 56;
/// `lwz r29,28(r4)` — the descriptor read from this block. Swapped with [`DEST_DESC`] on exit.
pub const SOURCE_DESC: u32 = 28;
/// `lwz r28,32(r4)` — the descriptor written this block.
pub const DEST_DESC: u32 = 32;

const _: () = assert!(CHANNEL_COUNT == crate::mix::CHANNELS, "the same count mix.rs reloads");
const _: () = assert!(SOURCE_DESC == crate::mix::PAIR_BACK && DEST_DESC == crate::mix::PAIR_FRONT);

// The per-sample step scale, a rodata single. `((lis_imm & 0xFFFF) << 16) + offsets`, computed:
// lis r11,-32208 -> 0x82300000 ; addi r10,r11,-31232 -> 0x822F8600 ; lfs f0,480(r10).
const LIS_82300000: u32 = ((-32208i32 as u32) & 0xFFFF) << 16;
const _: () = assert!(LIS_82300000 == 0x8230_0000, "lis -32208");

/// `lfs f0,480(r10)` — **measured 0.015625, i.e. 1/64**, which is what makes this a ramp over 64
/// samples: `crate::dsp::gain_ramp`'s own `RAMP_SPAN` cell measures 64.0 and its ramp covers the
/// first 64 of the block's 256 singles. Read live through the [`Guest`] map anyway.
pub const STEP_SCALE: u32 = LIS_82300000.wrapping_add(-31232i32 as u32) + 480;
const _: () = assert!(STEP_SCALE == 0x822F_87E0);
const _: () = assert!(STEP_SCALE == crate::spatial::POOL + 480, "the same pool, 480 in");

/// What [`ramp_channels`] leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RampResult {
    /// `li r3,1` — the guest's own result, and the mask is `kReturnR3`, so it **is** compared. It is
    /// unconditionally 1: the `li` sits in the tail that every path reaches.
    pub r3: u64,
    /// The `v28`-`v31` clobber of the **last** `sub_82B3C098` call, or `None` when the channel count
    /// was zero and no call was made.
    ///
    /// This function does not save those registers either — it uses `__savegprlr_26`, not
    /// `__savevmx_*` — so the callee's clobber reaches its own caller, and the harness compares
    /// `v28`-`v31` on every call regardless of the result mask. Dropping it would be a divergence on
    /// 292,165 calls a session.
    pub vector_clobbers: Option<GainRampClobbers>,
}

/// `sub_82B23B50` — ramp every channel of a double-buffered block, then swap the pair.
///
/// `object` is `r3` (the gain object), `pair` is `r4` (the descriptor pair) and `restart` is `r5`, of
/// which **only the low byte is read** (`clrlwi r11,r5,24`). A non-zero flag means "no ramp this
/// block": the applied gain is latched to the target up front, so the step below comes out zero.
/// The result is `r3 = 1` and the mask is `kReturnR3`.
///
/// **Writes** the applied gain at `+56` (twice on the restart path, once otherwise), the two swapped
/// descriptor pointers at `pair + 28`/`+32`, and one 1024-byte block per channel through
/// [`crate::dsp::gain_ramp::gain_ramp_copy`]. **Reads** the channel count, both gains, both
/// descriptors, [`STEP_SCALE`], one 1024-byte source block per channel, and — through the kernel,
/// which the C++ `Windows()` does not declare — the thirteen rodata cells
/// `crate::dsp::gain_ramp` names.
///
/// `step = (target − applied) · 1/64`, as `fsubs` then `fmuls`, **never fused**; it goes into `f2`
/// once and is reused for every channel, while `f1` is re-read from `+56` on each iteration. The
/// kernel clobbers `f0` and `f8`-`f13` but never `f1`/`f2`, so the original's reuse is sound. That is
/// the original's own assumption and it is reproduced rather than hardened.
///
/// One consequence of the latch worth stating: because `applied` is re-read inside the loop and only
/// written *after* it, every channel ramps from the same starting gain. A reader expecting the ramp
/// to accumulate across channels would be wrong.
pub fn ramp_channels(
    g: &mut Guest,
    object: u32,
    pair: u32,
    restart: u64,
) -> Result<RampResult> {
    let mut fpscr = Fpscr::capture();

    let restart = (restart & 0xFF) as u32; // clrlwi r11,r5,24
    let source = g.u32(pair + SOURCE_DESC)?; // lwz r29,28(r4)
    let dest = g.u32(pair + DEST_DESC)?; // lwz r28,32(r4)

    // cmplwi cr6,r11,0 ; beq cr6
    if restart != 0 {
        fpscr.disable_flush_mode_unconditional();
        let target = fp::load_single(g, object + TARGET_GAIN)?; // lfs f0,52(r3)
        fp::store_single(g, object + APPLIED_GAIN, target)?; // stfs f0,56(r3)
    }

    // loc_82B23B80: the ramp step, computed once for the whole loop.
    fpscr.disable_flush_mode_unconditional();
    let target = fp::load_single(g, object + TARGET_GAIN)?; // lfs f0,52(r30)
    let applied = fp::load_single(g, object + APPLIED_GAIN)?; // lfs f13,56(r30)
    let channels = u32::from(g.u8(object + CHANNEL_COUNT)?); // lbz r27,42(r30)
    let span = fp::sub_single(target, applied); // fsubs f12,f0,f13
    let scale = fp::load_single(g, STEP_SCALE)?; // lfs f0,480(r10)
    let step = fp::mul_single(span, scale); // fmuls f2,f12,f0 — f2, set once

    // cmplwi cr6,r27,0 ; beq cr6 — the count is pre-tested, so the do-while is exactly this loop.
    let mut vector_clobbers = None;
    for channel in 0..channels {
        // loc_82B23BAC.
        let source_stride = u32::from(g.u16(source + CHANNEL_STRIDE)?); // lhz r11,14(r29)
        fpscr.disable_flush_mode_unconditional();
        let gain = fp::load_single(g, object + APPLIED_GAIN)?; // lfs f1,56(r30) — re-read
        let dest_stride = u32::from(g.u16(dest + CHANNEL_STRIDE)?); // lhz r9,14(r28)
        let source_buffer = g.u32(source + BUFFER_BASE)?; // lwz r10,4(r29)
        let dest_buffer = g.u32(dest + BUFFER_BASE)?; // lwz r8,4(r28)
        // mullw ; rlwinm ; add — 64-bit end to end, truncated only where the callee truncates.
        let source_run = channel_address(source_stride, channel, source_buffer);
        let dest_run = channel_address(dest_stride, channel, dest_buffer);
        // bl 0x82b3c098 — one fixed 1024-byte block, gain-ramped.
        vector_clobbers = Some(gain_ramp::gain_ramp_copy(
            g,
            dest_run as u32,
            source_run as u32,
            gain,
            step,
        )?);
    }

    // loc_82B23BE8: swap the pair, then latch the gain. Both words are read before either store.
    let swapped_source = g.u32(pair + DEST_DESC)?; // lwz r11,32(r26)
    let r3: u64 = 1; // li r3,1 — between the two loads, as lifted
    let swapped_dest = g.u32(pair + SOURCE_DESC)?; // lwz r10,28(r26)
    g.set_u32(pair + SOURCE_DESC, swapped_source)?; // stw r11,28(r26)
    g.set_u32(pair + DEST_DESC, swapped_dest)?; // stw r10,32(r26)
    fpscr.disable_flush_mode_unconditional();
    let target = fp::load_single(g, object + TARGET_GAIN)?; // lfs f0,52(r30)
    fp::store_single(g, object + APPLIED_GAIN, target)?; // stfs f0,56(r30)

    Ok(RampResult { r3, vector_clobbers })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXER: u32 = 0x4000_0000;
    const DST_DESC: u32 = MIXER + 0x0400;
    const SRC_DESC: u32 = MIXER + 0x0410;
    const DST_BUF: u32 = MIXER + 0x1000;
    const SRC_BUF: u32 = MIXER + 0x9000;

    const RAMP_OBJ: u32 = MIXER + 0x0500;
    const PAIR: u32 = MIXER + 0x0600;
    const DESC_A: u32 = MIXER + 0x0700;
    const DESC_B: u32 = MIXER + 0x0710;

    /// A guest map with the work area plus every rodata cell the two kernels read: this module's own
    /// [`STEP_SCALE`] and the thirteen cells `crate::dsp::gain_ramp` names, all with the values read
    /// out of the validated image dump.
    ///
    /// The thirteen are listed here because **`sub_82B23B50`'s C++ `Windows()` does not declare any
    /// of them as reads** — it declares only its own `0x822F87E0`. A recorded vector for it is
    /// therefore missing the callee's constants, and replaying it would fail on the first cell rather
    /// than on a divergence.
    fn guest() -> Guest {
        use crate::dsp::gain_ramp as gr;
        let mut g = Guest::from_segments(vec![
            crate::Segment { base: MIXER, bytes: vec![0u8; 0x0002_0000] },
            crate::Segment { base: 0x8206_0000, bytes: vec![0u8; 0x4000] },
            crate::Segment { base: 0x8225_7000, bytes: vec![0u8; 0x1000] },
            crate::Segment { base: 0x820E_D000, bytes: vec![0u8; 0x1000] },
            crate::Segment { base: 0x8231_BA00, bytes: vec![0u8; 0x100] },
            crate::Segment { base: 0x822F_8600, bytes: vec![0u8; 0x1000] },
        ]);
        g.set_u32(STEP_SCALE, 0.015625f32.to_bits()).unwrap(); // measured 1/64
        g.set_u32(gr::STEP_SCALE, 4.0f32.to_bits()).unwrap();
        g.set_u32(gr::RAMP_SPAN, 64.0f32.to_bits()).unwrap();
        g.set_u32(gr::LANE2_SCALE, 2.0f32.to_bits()).unwrap();
        g.set_u32(gr::LANE3_SCALE, 3.0f32.to_bits()).unwrap();
        for group in 1..8u32 {
            splat(&mut g, gr::SCALE[group as usize], group as f32);
        }
        splat(&mut g, gr::SCALE_STEP, 8.0);
        g
    }

    fn splat(g: &mut Guest, base: u32, v: f32) {
        for i in 0..4 {
            g.set_u32(base + 4 * i, v.to_bits()).unwrap();
        }
    }

    fn fill(g: &mut Guest, base: u32, n: u32, v: f32) {
        for i in 0..n {
            g.set_u32(base + 4 * i, v.to_bits()).unwrap();
        }
    }

    fn at(g: &Guest, ea: u32) -> f32 {
        g.f32(ea).unwrap()
    }

    /// A descriptor: `+4` the buffer, `+14` the `u16` stride in singles.
    fn descriptor(g: &mut Guest, desc: u32, buffer: u32, stride: u16) {
        g.set_u32(desc + BUFFER_BASE, buffer).unwrap();
        g.set_u16(desc + CHANNEL_STRIDE, stride).unwrap();
    }

    fn gain(g: &mut Guest, source: u32, dest: u32, value: f32) {
        g.set_u32(MIXER + GAIN_MATRIX + GAIN_ROW_STRIDE * source + 4 * dest, value.to_bits())
            .unwrap();
    }

    // =========================================================== the address arithmetic

    #[test]
    fn a_channel_address_is_a_64_bit_sum_of_a_32_bit_offset() {
        // The offset is the low word of `stride*index`, scaled by four and masked: a multiple of 4,
        // always inside 32 bits.
        assert_eq!(channel_offset(256, 0), 0);
        assert_eq!(channel_offset(256, 1), 1024);
        assert_eq!(channel_offset(256, 3), 3072);
        // The `rlwinm` keeps only the low word, so a product that overflows 32 bits wraps rather
        // than carrying: stride 0x8000 by index 0x8000 is 2^30 words, i.e. 2^32 bytes, i.e. zero.
        assert_eq!(channel_offset(0x8000, 0x8000), 0);
        // And it is a shift, not a rotate. 0x8000 * 0x10001 has low word 0x80008000, whose top two
        // bits the mask discards: a rotate would bring them back in as 0x00020002.
        assert_eq!(channel_offset(0x8000, 0x1_0001), 0x0002_0000);

        // The sum is where the 64 bits are load-bearing. `CLAUDE.md`: a chain truncated to 32 bits
        // leaves memory byte-identical and the register wrong.
        let wide = channel_address(256, 1, 0xFFFF_FF00);
        assert_eq!(wide, 0x1_0000_0300, "the carry into bit 32 survives");
        assert_eq!(wide as u32, 0x0000_0300, "and the truncation is the callee's, not ours");
        assert!(wide > u64::from(u32::MAX), "a u32 chain could not represent this at all");
    }

    // =========================================================== sub_82B29AF0

    #[test]
    fn pass_one_overwrites_and_pass_two_accumulates() {
        // Two sources into two destinations. dst[d] must end up at src0*gain[0][d] + src1*gain[1][d],
        // and the pre-filled junk in dst proves the first pass *overwrote* rather than accumulated.
        let mut g = guest();
        g.set_u32(MIXER + SOURCE_COUNT, 2).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 2).unwrap();
        descriptor(&mut g, DST_DESC, DST_BUF, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        fill(&mut g, DST_BUF, 768, 7.0); // 256 words past the two channels, to catch an overrun
        fill(&mut g, SRC_BUF, 256, 1.0);
        fill(&mut g, SRC_BUF + 1024, 256, 2.0);
        gain(&mut g, 0, 0, 0.5);
        gain(&mut g, 0, 1, 0.25);
        gain(&mut g, 1, 0, 1.0);
        gain(&mut g, 1, 1, 2.0);

        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();

        // 1*0.5 + 2*1.0 and 1*0.25 + 2*2.0.
        for i in 0..256u32 {
            assert_eq!(at(&g, DST_BUF + 4 * i), 2.5, "dest 0 sample {i}");
            assert_eq!(at(&g, DST_BUF + 1024 + 4 * i), 4.25, "dest 1 sample {i}");
        }
        assert_eq!(at(&g, DST_BUF + 2048), 7.0, "nothing past the two channels");
    }

    #[test]
    fn a_single_source_channel_never_reaches_the_accumulating_pass() {
        // `cmplwi r10,1 ; ble` — a count of exactly 1 skips pass 2 entirely, so row 1 of the matrix
        // must never be read. Poison it with a value that would be obvious in the output.
        let mut g = guest();
        g.set_u32(MIXER + SOURCE_COUNT, 1).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 1).unwrap();
        descriptor(&mut g, DST_DESC, DST_BUF, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        fill(&mut g, SRC_BUF, 256, 1.0);
        fill(&mut g, SRC_BUF + 1024, 256, 1000.0);
        gain(&mut g, 0, 0, 0.5);
        gain(&mut g, 1, 0, 1000.0);
        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();
        assert_eq!(at(&g, DST_BUF), 0.5, "only source 0 and only gain[0][0]");
    }

    #[test]
    fn a_zero_destination_count_writes_nothing_at_all() {
        // Both passes are gated on it: pass 1 by `cmplwi r11,0 ; beq`, pass 2's inner loop by the
        // same test on the carried variable. A port that ran the inner loop unconditionally, or that
        // tested the source count first, would write here.
        let mut g = guest();
        g.set_u32(MIXER + SOURCE_COUNT, 3).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 0).unwrap();
        descriptor(&mut g, DST_DESC, DST_BUF, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        fill(&mut g, DST_BUF, 1024, 7.0);
        fill(&mut g, SRC_BUF, 1024, 1.0);
        // Every gain the two passes could reach is loud, so a single stray kernel call is visible.
        // With only gain[0][0] set the control "run pass 2's inner loop unconditionally" wrote an
        // accumulate of zero and this test passed; that is what a poisoned matrix is for.
        for s in 0..3u32 {
            for d in 0..2u32 {
                gain(&mut g, s, d, 5.0);
            }
        }
        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();
        for i in 0..1024u32 {
            assert_eq!(at(&g, DST_BUF + 4 * i), 7.0, "word {i}");
        }
    }

    #[test]
    fn the_destination_count_is_reloaded_after_every_kernel_call() {
        // Point the destination buffer at the count word itself. The first kernel call writes zeros
        // over it, the reload reads 0, and the loop stops after one channel — where an entry-once
        // read of 4 would have written three more channels.
        //
        // The buffer is deliberately 4-byte rather than 128-byte aligned, so the kernel takes its
        // scalar path and writes exactly [dst, dst+1024): a vector-path `dcbzl` would round the
        // start down to a 128-byte line and also clear the source count, which is not what this test
        // is about. The C++ `Windows()` declines this layout, so nothing here establishes what the
        // guest does with it — only that the reload survived translation.
        let mut g = guest();
        let dst_buf = MIXER + DEST_COUNT;
        assert_ne!(dst_buf & 0x7F, 0, "must take the kernel's scalar path");
        g.set_u32(MIXER + SOURCE_COUNT, 1).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 4).unwrap();
        descriptor(&mut g, DST_DESC, dst_buf, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        fill(&mut g, SRC_BUF, 256, 0.0);
        fill(&mut g, dst_buf + 1024, 1024, 7.0);
        gain(&mut g, 0, 0, 0.0);
        gain(&mut g, 0, 1, 5.0);
        gain(&mut g, 0, 2, 5.0);
        gain(&mut g, 0, 3, 5.0);
        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();
        assert_eq!(g.u32(MIXER + DEST_COUNT).unwrap(), 0, "the first channel cleared the count");
        for i in 0..1024u32 {
            assert_eq!(at(&g, dst_buf + 1024 + 4 * i), 7.0, "channel 1 word {i} was written");
        }
    }

    #[test]
    fn the_gain_cursor_walks_one_row_per_source_and_restarts_at_its_head() {
        // `addi r29,r3,440` with `lfsu`'s pre-increment puts gain[0][0] at +444, and pass 2 restarts
        // the cursor at `row - 4` for every source with `addi r24,r24,32` between them. Distinct
        // gains in every cell make a cursor that failed to restart, or that used the wrong stride,
        // produce the wrong sum.
        let mut g = guest();
        g.set_u32(MIXER + SOURCE_COUNT, 3).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 2).unwrap();
        descriptor(&mut g, DST_DESC, DST_BUF, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        for s in 0..3u32 {
            fill(&mut g, SRC_BUF + 1024 * s, 256, 1.0);
            for d in 0..2u32 {
                gain(&mut g, s, d, (1 + s * 2 + d) as f32); // 1,2 / 3,4 / 5,6
            }
        }
        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();
        assert_eq!(at(&g, DST_BUF), 1.0 + 3.0 + 5.0, "column 0");
        assert_eq!(at(&g, DST_BUF + 1024), 2.0 + 4.0 + 6.0, "column 1");
        // And the row stride really is 32 bytes: the eight words of row 0 end where row 1 begins.
        assert_eq!(MIXER + GAIN_MATRIX + GAIN_ROW_STRIDE, MIXER + 476);
    }

    // =========================================================== sub_82B23B50

    /// The gain object and the descriptor pair, with `channels` channels of 256 singles each.
    fn ramp_layout(g: &mut Guest, channels: u8, target: f32, applied: f32) {
        g.set_u8(RAMP_OBJ + CHANNEL_COUNT, channels).unwrap();
        g.set_u32(RAMP_OBJ + TARGET_GAIN, target.to_bits()).unwrap();
        g.set_u32(RAMP_OBJ + APPLIED_GAIN, applied.to_bits()).unwrap();
        g.set_u32(PAIR + SOURCE_DESC, DESC_A).unwrap();
        g.set_u32(PAIR + DEST_DESC, DESC_B).unwrap();
        descriptor(g, DESC_A, MIXER + 0x1_0000, 256);
        descriptor(g, DESC_B, MIXER + 0x1_8000, 256);
    }

    #[test]
    fn it_ramps_every_channel_then_swaps_the_pair_and_latches_the_gain() {
        let mut g = guest();
        ramp_layout(&mut g, 2, 1.0, 0.0);
        let src = MIXER + 0x1_0000;
        let dst = MIXER + 0x1_8000;
        fill(&mut g, src, 512, 1.0);
        fill(&mut g, dst + 2048, 256, 7.0);

        let out = ramp_channels(&mut g, RAMP_OBJ, PAIR, 0).unwrap();
        assert_eq!(out.r3, 1, "li r3,1 on every path");
        assert!(out.vector_clobbers.is_some(), "two calls were made, so v28-v31 moved");

        // step = (1.0 - 0.0)/64, so the kernel's gain is k/64 for the first 64 samples and then 1.0.
        for channel in 0..2u32 {
            let base = dst + 2048 * 0 + 1024 * channel;
            assert_eq!(at(&g, base), 0.0, "channel {channel} sample 0 takes the applied gain");
            assert_eq!(at(&g, base + 4), 1.0 / 64.0, "sample 1");
            assert_eq!(at(&g, base + 4 * 64), 1.0, "sample 64 holds at the target");
            assert_eq!(at(&g, base + 4 * 255), 1.0, "and so does the last");
        }
        assert_eq!(at(&g, dst + 2048), 7.0, "nothing past the two channels");

        // The pair is swapped and the gain latched.
        assert_eq!(g.u32(PAIR + SOURCE_DESC).unwrap(), DESC_B);
        assert_eq!(g.u32(PAIR + DEST_DESC).unwrap(), DESC_A);
        assert_eq!(at(&g, RAMP_OBJ + APPLIED_GAIN), 1.0, "applied := target");
    }

    #[test]
    fn every_channel_ramps_from_the_same_starting_gain() {
        // The applied gain is re-read inside the loop and only written after it, so channel 1 starts
        // where channel 0 did. A port that latched the gain before the loop, or inside it, would
        // give the second channel a flat copy.
        let mut g = guest();
        ramp_layout(&mut g, 2, 1.0, 0.0);
        let src = MIXER + 0x1_0000;
        let dst = MIXER + 0x1_8000;
        fill(&mut g, src, 512, 1.0);
        ramp_channels(&mut g, RAMP_OBJ, PAIR, 0).unwrap();
        assert_eq!(at(&g, dst), at(&g, dst + 1024), "both channels start at 0.0");
        assert_eq!(at(&g, dst + 4), at(&g, dst + 1024 + 4));
    }

    #[test]
    fn the_restart_flag_latches_the_gain_first_so_the_step_comes_out_zero() {
        // Non-zero means "no ramp this block": applied := target before the step is computed, so the
        // span is zero and every sample takes the target gain.
        let mut g = guest();
        ramp_layout(&mut g, 1, 1.0, 0.0);
        let src = MIXER + 0x1_0000;
        let dst = MIXER + 0x1_8000;
        fill(&mut g, src, 256, 1.0);
        ramp_channels(&mut g, RAMP_OBJ, PAIR, 1).unwrap();
        for i in [0u32, 1, 32, 64, 255] {
            assert_eq!(at(&g, dst + 4 * i), 1.0, "sample {i} is flat at the target");
        }
    }

    #[test]
    fn only_the_low_byte_of_the_flag_is_read() {
        // `clrlwi r11,r5,24`. 0x100 has no low byte set, so it must behave exactly like 0 — and the
        // ramp has to be visible, which is why sample 0 is checked rather than the latch (the latch
        // happens on both paths).
        let mut g = guest();
        ramp_layout(&mut g, 1, 1.0, 0.0);
        let src = MIXER + 0x1_0000;
        let dst = MIXER + 0x1_8000;
        fill(&mut g, src, 256, 1.0);
        ramp_channels(&mut g, RAMP_OBJ, PAIR, 0x100).unwrap();
        assert_eq!(at(&g, dst), 0.0, "0x100 is a zero flag, so the ramp runs");
        assert_eq!(at(&g, dst + 4), 1.0 / 64.0);

        // And 0x101 is not.
        let mut h = guest();
        ramp_layout(&mut h, 1, 1.0, 0.0);
        fill(&mut h, src, 256, 1.0);
        ramp_channels(&mut h, RAMP_OBJ, PAIR, 0x101).unwrap();
        assert_eq!(at(&h, dst), 1.0, "0x101 does have a low byte");
    }

    #[test]
    fn a_zero_channel_count_still_swaps_the_pair_and_latches_the_gain() {
        // The tail is unconditional, and the clobber report has to say that no kernel call was made
        // rather than inventing a vector register state.
        let mut g = guest();
        ramp_layout(&mut g, 0, 0.75, 0.0);
        let dst = MIXER + 0x1_8000;
        fill(&mut g, dst, 256, 7.0);
        let out = ramp_channels(&mut g, RAMP_OBJ, PAIR, 0).unwrap();
        assert_eq!(out.r3, 1);
        assert_eq!(out.vector_clobbers, None, "no call, so nothing to report");
        assert_eq!(at(&g, dst), 7.0, "no channel was copied");
        assert_eq!(g.u32(PAIR + SOURCE_DESC).unwrap(), DESC_B);
        assert_eq!(at(&g, RAMP_OBJ + APPLIED_GAIN), 0.75);
    }

    #[test]
    fn the_step_scale_cell_is_read_live_and_is_one_sixty_fourth() {
        // Measured 0.015625, which is what makes the ramp span 64 samples — the same 64 the kernel's
        // own `RAMP_SPAN` cell holds. Patch the cell and the step follows it, because the body loads
        // it rather than folding it in.
        let mut g = guest();
        assert_eq!(at(&g, STEP_SCALE), 0.015625);
        ramp_layout(&mut g, 1, 1.0, 0.0);
        let src = MIXER + 0x1_0000;
        let dst = MIXER + 0x1_8000;
        fill(&mut g, src, 256, 1.0);
        g.set_u32(STEP_SCALE, 0.03125f32.to_bits()).unwrap(); // 1/32
        ramp_channels(&mut g, RAMP_OBJ, PAIR, 0).unwrap();
        assert_eq!(at(&g, dst + 4), 1.0 / 32.0, "the step doubled with the cell");
    }

    #[test]
    fn both_bodies_restore_the_entry_flush_mode() {
        let mut g = guest();
        g.set_u32(MIXER + SOURCE_COUNT, 2).unwrap();
        g.set_u32(MIXER + DEST_COUNT, 1).unwrap();
        descriptor(&mut g, DST_DESC, DST_BUF, 256);
        descriptor(&mut g, SRC_DESC, SRC_BUF, 256);
        ramp_layout(&mut g, 1, 1.0, 0.0);
        let before = crate::vmx::get_mxcsr();
        apply_gain_matrix(&mut g, MIXER, DST_DESC, SRC_DESC).unwrap();
        assert_eq!(crate::vmx::get_mxcsr(), before, "apply_gain_matrix");
        ramp_channels(&mut g, RAMP_OBJ, PAIR, 0).unwrap();
        assert_eq!(crate::vmx::get_mxcsr(), before, "ramp_channels");
    }
}
