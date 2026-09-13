//! Four small verified leaves, each reached from a different mechanism.
//!
//! They share a file because none of them belongs to a larger one that is ported yet, and a
//! module per two-instruction function would hide them rather than organise them. Each is
//! ported from its `recomp/src/audio_ports/sub_*.inc`, all four **STATUS: verified** —
//! compared against the original body call for call under the shadow harness.
//!
//! | function | guest | lifted lines | calls/boot | calls/play | reached from |
//! |---|---|---|---|---|---|
//! | [`stamp_slot`] | `sub_82B463A8` | 31 | 761,054 | 1,137,330 | a command-ring record's handler slot |
//! | [`set_field_460`] | `sub_82B34268` | 11 | 202,510 | 304,676 | a direct `bl` |
//! | [`fourth_argument`] | `sub_82B2C8E8` | 7 | 123,881 | 225,599 | a pointer slot only |
//! | [`stream_remaining`] | `sub_82B23C10` | 47 | 122,997 | 214,853 | five direct callers |
//!
//! **Replayed against the game, 2026-09-13: 6,661 recorded vectors, 0 disagreements**
//! (3,232 + 230 + 230 + 2,969, sessions `leaves` and `leaves2`). The four were recorded on purpose
//! — no earlier capture contained them — by naming them in `AUDIO_VECTORS_ONLY`, and the same
//! sessions compared them live against the original 672,366 / 161,603 / 99,945 / 97,943 times with
//! zero divergence. One limit: the recording keeps only the low word of `r3`, so
//! [`stream_remaining`]'s borrow into the upper word is checked by the tests below and not by any
//! vector.
//!
//! Two of the four objects are unnamed. `docs/rw_audio_structs.h` has no entry for the slot
//! array at `+0x10`, the `u16` at `+460`, or the stream table [`stream_remaining`] walks, so
//! every offset below is a plain constant with the instruction that produced it beside it.

use crate::{fp, Guest, Result};

/// A command-ring record: `{+0 handler, +4 object, +8 index, +12 value}`, 16 bytes.
///
/// The producer at `skate3_recomp.11.cpp:59366` stores [`stamp_slot`]'s own address at `+0` and
/// advances the ring by 16; the drain advances by whatever the handler returns, which is why
/// [`stamp_slot`] returns [`RECORD_SIZE`] rather than a status.
pub const RECORD_OBJECT: u32 = 4;
/// Slot index, `u32`. Scaled by 8 in 32 bits — see [`stamp_slot`].
pub const RECORD_INDEX: u32 = 8;
/// The value to stamp, stored as a single.
pub const RECORD_VALUE: u32 = 12;
/// `li r3,16` — the record size the drain advances by.
pub const RECORD_SIZE: u32 = 16;

/// `object + 0x10` holds the **base of** an array of 8-byte slots `{marker u32, value f32}`.
pub const OBJECT_SLOTS: u32 = 16;
/// One slot: `{+0 marker u32, +4 value f32}`.
pub const SLOT_STRIDE: u32 = 8;

/// `lis r10,32759 ; ori r8,r10,65521` — computed as `((imm & 0xFFFF) << 16) | imm`, never read
/// off. The same NaN payload `docs/command-queue.md` sees stamped by the query path of
/// `sub_82B28A00`; here it sits beside an arbitrary float rather than a 0.0/1.0 constant, so what
/// it *means* is not settled. Reproduced, not interpreted.
pub const SLOT_MARKER: u32 = ((32759u32) << 16) | 65521u32;

const _: () = assert!(SLOT_MARKER == 0x7FF7_FFF1, "lis 32759 ; ori 65521");

/// `sub_82B34268`'s only store. Nothing names `+460` (`0x1CC`); `sub_82B29278` is the same shape
/// at `+364`.
pub const FIELD_460: u32 = 460;

/// Stamp `{`[`SLOT_MARKER`]`, value}` into slot `index` of the record's object, and return the
/// record size (`sub_82B463A8`).
///
/// The hottest command handler seen on `RwAudioCore Dac`: 1,137,330 calls in a played session. It
/// is never the target of a `bl` anywhere in the corpus — its address only ever appears as a
/// record's `+0`, so it runs through the ring's dispatch.
///
/// Three details are load-bearing, and each is the reason a plausible rewrite would be wrong:
///
/// - **The value round-trips through a double.** `lfs f0,12(r3)` widens the single, `stfs f0,4(r11)`
///   narrows it back. That is value-preserving for every finite single, and *not* for a signalling
///   NaN, which comes back quiet, and not for a denormal, which is **flushed to zero**. The guest
///   emits `disableFlushModeUnconditional` at the load, and that does not mean what it says:
///   both of RexGlue's modes carry `FZ|DAZ` (see [`crate::vmx::Fpscr`]), so `DAZ` reads the
///   denormal single as zero on the way in. That is exactly why this function is x86-only here and
///   holds an [`Fpscr`](crate::vmx::Fpscr) — a build without one would run under Rust's default
///   MXCSR and *preserve* a denormal the recomp destroys.
/// - **`index << 3` is 32-bit.** The `rlwinm r10,r9,3,0,28` drops the top three bits of the index,
///   so an index at or above 2^29 wraps — identically in both bodies.
/// - **Store order is value then marker.** A concurrent reader can see the slot half-written, and
///   which half it sees is observable. Preserved, not tidied.
///
/// An object of 0, or a slot base of 0, returns `Err` here: the store would go through a
/// null-based address. The harness declines those calls too (`Windows()` returns false), so no
/// recorded vector can contain one, and this is the one input where the two bodies part company.
#[cfg(target_arch = "x86_64")]
pub fn stamp_slot(g: &mut Guest, record: u32) -> Result<u64> {
    let object = g.u32(record + RECORD_OBJECT)?; // lwz r11,4(r3)
    let index = g.u32(record + RECORD_INDEX)?; // lwz r9,8(r3)

    let mut fpscr = crate::vmx::Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // emitted at lfs f0,12(r3)
    let value = fp::single_from_bits(g.u32(record + RECORD_VALUE)?); // lfs f0,12(r3)

    let slots = g.u32(object + OBJECT_SLOTS)?; // lwz r11,16(r11)
    let slot = slots.wrapping_add(index << 3); // add r11,r11,r10

    fp::store_single(g, slot + 4, value)?; // stfs f0,4(r11)
    g.set_u32(slot, SLOT_MARKER)?; // stw r8,0(r11)
    drop(fpscr);
    Ok(u64::from(RECORD_SIZE)) // li r3,16
}

/// Store `value` as a `u16` at `object + 460`, and return 0 (`sub_82B34268`).
///
/// Eleven lifted lines, two of which do anything: `sth r6,460(r11)` and `li r3,0`. The value is
/// the **fourth** argument in the guest — `r4` and `r5` are untouched — which is the kind of
/// detail a rewrite from the role line alone gets wrong.
pub fn set_field_460(g: &mut Guest, object: u32, value: u16) -> Result<u64> {
    g.set_u16(object + FIELD_460, value)?; // sth r6,460(r11)
    Ok(0) // li r3,0
}

/// Return the fourth argument unchanged (`sub_82B2C8E8`).
///
/// `mr r3,r6 ; blr`, and nothing else: no loads, no stores, no branches, no condition register.
/// It is here because it is not *nothing* — 225,599 calls in a played session, reached only
/// through a pointer slot, and the move is **64-bit**, so whatever a caller left in the upper half
/// of `r6` is carried into `r3` verbatim. A `u32` signature would be a divergence the first time a
/// caller left junk above bit 31, which is why this takes and returns `u64`.
///
/// What it is *for* is unknown. Its neighbours look like slots of one small class — `82B2C8C0` is
/// `li r3,44`, a size — but nothing here verifies that, so the description says only what the two
/// instructions say.
pub fn fourth_argument(r6: u64) -> u64 {
    r6 // mr r3,r6
}

/// `+28` — the live cursor of whichever stream is currently active.
pub const ACTIVE_CURSOR: u32 = 28;
/// `+36` — a byte offset **from the object**, not a pointer, of the element array.
pub const STREAM_TABLE: u32 = 36;
/// `+49` — the index of the currently active stream, one byte.
pub const ACTIVE_INDEX: u32 = 49;
/// The element array's stride.
pub const ELEMENT_STRIDE: u32 = 24;
/// Per element: `+8` that stream's saved cursor, stale while it is the active one.
pub const ELEMENT_CURSOR: u32 = 8;
/// Per element: `+12` its limit. Zero means the element is unbound.
pub const ELEMENT_LIMIT: u32 = 12;

/// Address of element `index`, from entry state alone.
///
/// `rlwinm r11,r4,1,23,30 ; add r11,r10,r11 ; rlwinm r11,r11,3,0,28` is exactly `24 * index` for
/// an index of 255 or less, which the `clrlwi` guarantees. The lifted adds are 64-bit and the load
/// truncates to 32, and addition modulo 2^32 is associative, so `u32` arithmetic here is the same
/// effective address rather than an approximation of one.
pub fn element_address(g: &Guest, object: u32, index: u8) -> Result<u32> {
    let table = g.u32(object + STREAM_TABLE)?; // lwz r9,36(r3)
    Ok(object
        .wrapping_add(table)
        .wrapping_add(u32::from(index) * ELEMENT_STRIDE))
}

/// How much of stream `index` is unconsumed: its limit minus its cursor (`sub_82B23C10`).
///
/// Reached as `Player + 0x150` (`rw_player.decoder`) from `sub_82B29288`, with the index byte
/// coming out of the Player's `+0x54` table. Only the low byte of the guest's `r4` is used
/// (`clrlwi r10,r4,24`), which is why this takes a `u8`.
///
/// Three behaviours worth stating, because each is a branch a rewrite can flatten:
///
/// - **An unbound element short-circuits.** Limit 0 returns 0 without reading the cursor at all —
///   before the active-index test, so a zero limit wins even for the active stream.
/// - **The active stream's saved cursor is stale.** When `index` equals the byte at `+49`, the
///   cursor comes from the object's `+28`, not from the element. The comparison is unsigned on two
///   zero-extended bytes.
/// - **The subtraction is 64-bit, and it can borrow.** `subf r3,r10,r11` on two zero-extended
///   words, so a cursor past the limit leaves `0xFFFF_FFFF_xxxx_xxxx` in `r3` — which the callers
///   read back as a negative `i32`. Returning `u32` here would erase that, so this returns `u64`.
pub fn stream_remaining(g: &Guest, object: u32, index: u8) -> Result<u64> {
    let element = element_address(g, object, index)?;

    let limit = u64::from(g.u32(element + ELEMENT_LIMIT)?); // lwz r11,12(r9)
    if limit == 0 {
        return Ok(0); // cmpwi cr6,r11,0 ; li r3,0
    }

    let active = g.u8(object + ACTIVE_INDEX)?; // lbz r8,49(r3)
    let cursor = if index == active {
        // cmplw cr6,r10,r8 -- the live cursor, the element's copy being stale
        u64::from(g.u32(object + ACTIVE_CURSOR)?) // lwz r10,28(r3)
    } else {
        u64::from(g.u32(element + ELEMENT_CURSOR)?) // lwz r10,8(r9)
    };

    Ok(limit.wrapping_sub(cursor)) // subf r3,r10,r11
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x4000_0000;
    const OBJECT: u32 = BASE + 0x100;
    const SLOTS: u32 = BASE + 0x400;
    const RECORD: u32 = BASE + 0x40;
    const POISON: u32 = 0xDEAD_BEEF;

    fn guest() -> Guest {
        let mut g = Guest::single(BASE, 0x1000);
        for i in 0..0x400 / 4 {
            g.set_u32(SLOTS + i * 4, POISON).unwrap();
        }
        g.set_u32(OBJECT + OBJECT_SLOTS, SLOTS).unwrap();
        g
    }

    fn record(g: &mut Guest, index: u32, value_bits: u32) {
        g.set_u32(RECORD, 0x82B4_63A8).unwrap(); // the handler pointer the producer writes
        g.set_u32(RECORD + RECORD_OBJECT, OBJECT).unwrap();
        g.set_u32(RECORD + RECORD_INDEX, index).unwrap();
        g.set_u32(RECORD + RECORD_VALUE, value_bits).unwrap();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_marker_and_the_value_land_in_the_indexed_slot() {
        for index in [0u32, 1, 7, 63] {
            let mut g = guest();
            record(&mut g, index, 0.375f32.to_bits());

            assert_eq!(stamp_slot(&mut g, RECORD).unwrap(), 16);

            let slot = SLOTS + index * SLOT_STRIDE;
            assert_eq!(g.u32(slot).unwrap(), 0x7FF7_FFF1, "index {index}: marker");
            assert_eq!(g.f32(slot + 4).unwrap(), 0.375, "index {index}: value");
            // Nothing outside the eight bytes, which is the whole declared window.
            if index > 0 {
                assert_eq!(g.u32(slot - 4).unwrap(), POISON, "index {index}: below");
            }
            assert_eq!(g.u32(slot + SLOT_STRIDE).unwrap(), POISON, "index {index}: above");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_index_is_scaled_by_eight_in_thirty_two_bits() {
        // The `rlwinm ...,3,0,28` drops the top three bits, so 2^29 aliases index 0 exactly. A
        // 64-bit scale would put this store 4 GB away and the test would fail on the mapping.
        let mut g = guest();
        record(&mut g, 1 << 29, 1.0f32.to_bits());
        stamp_slot(&mut g, RECORD).unwrap();
        assert_eq!(g.u32(SLOTS).unwrap(), 0x7FF7_FFF1);
        assert_eq!(g.f32(SLOTS + 4).unwrap(), 1.0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_value_round_trips_through_a_double() {
        // Every finite single survives `lfs` then `stfs` exactly, which is what this asserts and all
        // it asserts.
        let mut g = guest();
        for bits in [0.375f32.to_bits(), 1.0f32.to_bits(), (-2.5e30f32).to_bits(), 0x0000_0000] {
            record(&mut g, 2, bits);
            stamp_slot(&mut g, RECORD).unwrap();
            assert_eq!(g.u32(SLOTS + 2 * SLOT_STRIDE + 4).unwrap(), bits);
        }

        // **What a denormal or a signalling NaN does here is a property of the build, not of this
        // port, and it is not settled.** Measured 2026-09-13 with `examples/flush_probe`: LLVM folds
        // `fptrunc(fpext(x))` into a no-op at `opt-level >= 1`, so the two conversions are simply
        // deleted and neither `DAZ` nor the hardware's sNaN quieting happens. At `opt-level = 0`
        // both execute, a denormal becomes `+0` and `0x7FA00000` comes back `0x7FE00000`. The recomp
        // is built `-O3` and its lifted body has the same `double(f32)`/`float(f64)` pair, so it is
        // very likely folded there too — but that was not measured, and no recorded vector for this
        // function carries a denormal or an sNaN, so the harness never decided it either. Asserting
        // either answer would be asserting this crate's profile flags.
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_null_object_is_refused_before_anything_is_written() {
        let mut g = guest();
        record(&mut g, 0, 1.0f32.to_bits());
        g.set_u32(RECORD + RECORD_OBJECT, 0).unwrap();
        assert!(stamp_slot(&mut g, RECORD).is_err());
        assert_eq!(g.u32(SLOTS).unwrap(), POISON, "nothing written");
    }

    #[test]
    fn the_field_at_460_is_two_bytes_big_endian_and_the_result_is_zero() {
        let mut g = Guest::single(BASE, 0x400);
        g.set_u32(BASE + 456, POISON).unwrap();
        g.set_u32(BASE + FIELD_460, POISON).unwrap();
        g.set_u32(BASE + 464, POISON).unwrap();

        assert_eq!(set_field_460(&mut g, BASE, 0x1234).unwrap(), 0);

        assert_eq!(g.u16(BASE + FIELD_460).unwrap(), 0x1234);
        // Two bytes, not four: the low half of that word is untouched.
        assert_eq!(g.u16(BASE + FIELD_460 + 2).unwrap(), 0xBEEF);
        assert_eq!(g.u32(BASE + 456).unwrap(), POISON);
        assert_eq!(g.u32(BASE + 464).unwrap(), POISON);
    }

    #[test]
    fn the_fourth_argument_comes_back_with_its_upper_half() {
        assert_eq!(fourth_argument(0x1234_5678_9ABC_DEF0), 0x1234_5678_9ABC_DEF0);
        // The case a u32 port gets wrong: junk above bit 31 is part of the result.
        assert_eq!(fourth_argument(0xFFFF_FFFF_0000_0001), 0xFFFF_FFFF_0000_0001);
        assert_eq!(fourth_argument(0), 0);
    }

    const DECODER: u32 = BASE + 0x200;
    const TABLE_OFFSET: u32 = 0x80; // +36 holds an offset from the object, not a pointer

    fn decoder(active: u8, live_cursor: u32) -> Guest {
        let mut g = Guest::single(BASE, 0x2000);
        g.set_u32(DECODER + STREAM_TABLE, TABLE_OFFSET).unwrap();
        g.set_u8(DECODER + ACTIVE_INDEX, active).unwrap();
        g.set_u32(DECODER + ACTIVE_CURSOR, live_cursor).unwrap();
        g
    }

    fn element(g: &mut Guest, index: u8, saved_cursor: u32, limit: u32) {
        let e = DECODER + TABLE_OFFSET + u32::from(index) * ELEMENT_STRIDE;
        g.set_u32(e + ELEMENT_CURSOR, saved_cursor).unwrap();
        g.set_u32(e + ELEMENT_LIMIT, limit).unwrap();
    }

    #[test]
    fn an_unbound_element_is_zero_even_when_it_is_the_active_stream() {
        let mut g = decoder(2, 4096);
        element(&mut g, 2, 7, 0);
        // The limit test comes first, so the live cursor is never read. If the order were the
        // other way round this would return a huge borrowed value instead.
        assert_eq!(stream_remaining(&g, DECODER, 2).unwrap(), 0);
    }

    #[test]
    fn the_active_stream_uses_the_live_cursor_and_the_others_their_saved_one() {
        let mut g = decoder(1, 1000);
        element(&mut g, 1, 7, 4096); // saved cursor 7 is stale for the active stream
        element(&mut g, 2, 512, 4096);

        assert_eq!(stream_remaining(&g, DECODER, 1).unwrap(), 4096 - 1000);
        assert_eq!(stream_remaining(&g, DECODER, 2).unwrap(), 4096 - 512);
        // The distinguishing part: reading the element's cursor for the active stream would give
        // 4089 here, which is a plausible answer and the wrong one.
        assert_ne!(stream_remaining(&g, DECODER, 1).unwrap(), 4096 - 7);
    }

    #[test]
    fn a_cursor_past_the_limit_borrows_into_the_upper_word() {
        let mut g = decoder(0, 0);
        element(&mut g, 5, 5000, 4096);
        // subf on two zero-extended words: 4096 - 5000 as a 64-bit subtract, so the borrow fills
        // the upper word. The callers read the low word back as a negative i32.
        let r = stream_remaining(&g, DECODER, 5).unwrap();
        assert_eq!(r, 0xFFFF_FFFF_FFFF_FC78);
        assert_eq!(r as u32 as i32, -904);
    }

    #[test]
    fn the_element_stride_is_twenty_four_and_the_index_reaches_255() {
        let mut g = Guest::single(BASE, 0x2000);
        g.set_u32(DECODER + STREAM_TABLE, TABLE_OFFSET).unwrap();
        g.set_u8(DECODER + ACTIVE_INDEX, 0).unwrap();
        for index in [0u8, 1, 2, 255] {
            // A stride of 16 or 32 would read a neighbour's words, and every limit here differs.
            element(&mut g, index, 0, 100 + u32::from(index));
        }
        for index in [1u8, 2, 255] {
            assert_eq!(
                stream_remaining(&g, DECODER, index).unwrap(),
                u64::from(100 + u32::from(index)),
                "index {index}"
            );
        }
        assert_eq!(
            element_address(&g, DECODER, 255).unwrap(),
            DECODER + TABLE_OFFSET + 255 * 24
        );
    }
}
