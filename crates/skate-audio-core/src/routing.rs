//! `sub_82B426D0` — the scatter-mixer: run a table of route bytes, then zero what no route wrote.
//!
//! Ported from `recomp/src/audio_ports/sub_82B426D0.inc`, **STATUS: verified** — 83,885 calls in a
//! played session on `RwAudioCore Dac`, compared against the original under the shadow harness.
//!
//! Replayed against **500 recorded calls, 0 disagreements**, and compared live against the original
//! 17,788 times in the same session. Recording it needed the harness's read-span cap raised from 32
//! to 192 — it declares 64 spans, and the excess used to be dropped silently, which made every
//! recorded vector unreplayable while the shadow comparison stayed green (`docs/port-loop.md`).
//!
//! This is the first port here that is mostly *composition*: every sample it moves is moved by
//! [`crate::dsp::scale`]'s two kernels, and every byte it clears is cleared by [`crate::mem::memset`],
//! all three of which are verified and replayed in their own right. So the risk that lives here is
//! the decoding and the bookkeeping, and that is what the tests below aim at.
//!
//! ## The route byte
//!
//! One byte per route, and it packs three fields — which a reader would not guess from the three
//! `rlwinm`s that extract them, since each produces a *byte offset* rather than an index:
//!
//! | bits | field | extracted as |
//! |---|---|---|
//! | 2:0 | destination slot | `(route & 7) << 2` |
//! | 5:3 | source slot | `(route >> 1) & 0x1C` |
//! | 7:6 | gain | `(route >> 4) & 0xC`, a word index into [`GAIN_TABLE`] |
//!
//! ## The bookkeeping, which is the whole function
//!
//! Eight flag bytes say whether a destination has been written yet in *this call*. The first route
//! to reach a destination **overwrites** it (`sub_82B3BED8`, `dst[i] = src[i] * gain`) and sets the
//! flag; every later route to the same destination **accumulates** (`sub_82B44B20`,
//! `dst[i] += src[i] * gain`). Then a closing loop zeroes every destination slot the caller owns
//! whose flag is still clear. That is what makes the mixer's output independent of what was left in
//! those buffers by the previous block.
//!
//! Three details are reproduced rather than tidied, each because the original does it:
//!
//! - **The range's last index is re-read from memory every iteration**, so a route that wrote over
//!   `range + 1` would change its own trip count. (The C++ window builder refuses such a call; here
//!   the read simply happens where the original has it.)
//! - **The route byte is re-read after the overwriting call**, and it is the *reloaded* value that
//!   picks which flag gets set.
//! - **The flags live in the function's own frame.** `sp+80..87`, cleared by eight `stbu`s, and no
//!   callee is ever given their address — so a local array is the same machine, and this port takes
//!   no `sp` argument. With more than eight slots the original reads frame bytes it never wrote,
//!   which is uninitialised stack; the C++ declines those calls and so does this ([`Error`]).

use crate::{dsp, mem, Error, Guest, Result};

/// `lis r11,-32241 ; addi r24,r11,-10560` — four floats at `0x820ED6C0`, indexed by the route's
/// top two bits. Computed from the immediates, not read off.
pub const GAIN_TABLE: u32 = (((-32241i32 as u32) & 0xFFFF) << 16).wrapping_sub(10560);
const _: () = assert!(GAIN_TABLE == 0x820E_D6C0, "lis -32241 ; addi -10560");

/// `lbz r31,0(r25)` — the first route index, inclusive.
pub const RANGE_FIRST: u32 = 0;
/// `lbz r11,1(r25)` — the last route index, inclusive, and re-read every iteration.
pub const RANGE_LAST: u32 = 1;
/// The eight frame flag bytes, and so the most destination slots a call may own.
pub const FLAG_COUNT: u32 = 8;

/// The destination slot a route names: bits 2:0.
pub fn dest_slot(route: u8) -> u32 {
    u32::from(route) & 7 // clrlwi r9,r11,29
}
/// The source slot: bits 5:3, extracted as a byte offset by `rlwinm r10,r11,31,27,29`.
pub fn source_slot(route: u8) -> u32 {
    ((u32::from(route) >> 1) & 0x1C) >> 2
}
/// The gain word: bits 7:6, extracted as a byte offset by `rlwinm r8,r11,28,4,29`.
pub fn gain_slot(route: u8) -> u32 {
    ((u32::from(route) >> 4) & 0xC) >> 2
}

/// Run the routing table, then clear every destination no route wrote (`sub_82B426D0`).
///
/// Arguments are the guest's `r3`…`r8`: the eight-entry destination pointer array, the eight-entry
/// source pointer array, how many destination slots the caller owns, the float count per buffer, the
/// two-byte inclusive route range, and the route table itself. The original returns nothing.
///
/// Refuses, before touching anything, a `slot_count` above eight — see the module note.
pub fn scatter_mix(
    g: &mut Guest,
    dest_array: u32,
    source_array: u32,
    slot_count: u32,
    count: u32,
    range: u32,
    table: u32,
) -> Result<()> {
    if slot_count > FLAG_COUNT {
        // The original would read flag bytes its eight `stbu`s never wrote — uninitialised frame.
        return Err(Error::new(
            range,
            "scatter_mix: more destination slots than the original's eight frame flags",
        ));
    }

    // li r11,8 ; addi r10,r1,79 ; stbu r9,1(r10) x8
    let mut written = [0u8; FLAG_COUNT as usize];

    // lbz r31,0(r25) ; lbz r11,1(r25) ; cmplw ; bgt -- a do-while, so first > last runs nothing.
    let mut index = u32::from(g.u8(range + RANGE_FIRST)?);
    while index <= u32::from(g.u8(range + RANGE_LAST)?) {
        let route = g.u8(table + index)?; // lbzx r11,r31,r30
        let flag = written[dest_slot(route) as usize]; // lbzx r7,r9,r10
        let gain = crate::fp::load_single(g, GAIN_TABLE + gain_slot(route) * 4)?; // lfsx f1,r8,r24
        let src = g.u32(source_array + source_slot(route) * 4)?; // lwzx r4,r10,r28
        let dst = g.u32(dest_array + dest_slot(route) * 4)?; // lwzx r3,r9,r29

        if flag != 0 {
            dsp::scale::scale_accumulate(g, dst, src, count, gain)?; // bl 0x82b44b20
        } else {
            dsp::scale::scale(g, dst, src, count, gain)?; // bl 0x82b3bed8
            // lbzx r7,r31,r30 -- the route byte is reloaded after the call, and it is the reloaded
            // value that picks the flag.
            let reloaded = g.u8(table + index)?;
            written[dest_slot(reloaded) as usize] = 1; // li r26,1 ; stbx r26,r6,r8
        }

        index += 1; // addi r31,r31,1
    }

    // loc_82B42788: every slot the caller owns whose flag is still clear is zeroed.
    for slot in 0..slot_count {
        if written[slot as usize] == 0 {
            let buffer = g.u32(dest_array + slot * 4)?; // lwz r3,0(r30)
            // rlwinm r5,r27,2,0,29 -- the byte length, truncated to a multiple of four in 32 bits.
            let bytes = u64::from((count << 2) & 0xFFFF_FFFC);
            mem::memset(g, buffer, 0, bytes)?; // bl 0x82ee5e80
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x4000_0000;
    const DEST_ARRAY: u32 = BASE + 0x40;
    const SOURCE_ARRAY: u32 = BASE + 0x80;
    const RANGE: u32 = BASE + 0xC0;
    const TABLE: u32 = BASE + 0xD0;
    const BUFFERS: u32 = BASE + 0x1000;
    /// One buffer per slot, well clear of the next.
    const STRIDE: u32 = 0x200;
    const COUNT: u32 = 8;
    const POISON: u32 = 0xDEAD_BEEF;

    /// Four distinguishable gains, so a wrong table index is a wrong answer rather than a tie.
    const GAINS: [f32; 4] = [1.0, 2.0, 4.0, 8.0];

    /// Destination slots 0..4 at `BUFFERS`, source slots 0..4 after them.
    fn guest() -> Guest {
        let mut g = Guest::single(BASE, 0x4000);
        let mut bytes = Vec::new();
        for gain in GAINS {
            bytes.extend_from_slice(&gain.to_bits().to_be_bytes());
        }
        g.put(GAIN_TABLE, bytes);
        for slot in 0..4u32 {
            let dst = BUFFERS + slot * STRIDE;
            let src = BUFFERS + (4 + slot) * STRIDE;
            g.set_u32(DEST_ARRAY + slot * 4, dst).unwrap();
            g.set_u32(SOURCE_ARRAY + slot * 4, src).unwrap();
            for i in 0..COUNT {
                g.set_u32(dst + i * 4, POISON).unwrap();
                // Source slot s holds s+1, s+2, ... so a swapped source is visible immediately.
                g.set_u32(src + i * 4, ((slot + 1) as f32 + i as f32).to_bits()).unwrap();
            }
        }
        g
    }

    fn route(g: &mut Guest, bytes: &[u8]) {
        g.set_u8(RANGE + RANGE_FIRST, 0).unwrap();
        g.set_u8(RANGE + RANGE_LAST, (bytes.len() - 1) as u8).unwrap();
        for (i, b) in bytes.iter().enumerate() {
            g.set_u8(TABLE + i as u32, *b).unwrap();
        }
    }

    fn run(g: &mut Guest, slots: u32) {
        scatter_mix(g, DEST_ARRAY, SOURCE_ARRAY, slots, COUNT, RANGE, TABLE).unwrap();
    }

    fn out(g: &Guest, slot: u32) -> Vec<f32> {
        (0..COUNT).map(|i| g.f32(BUFFERS + slot * STRIDE + i * 4).unwrap()).collect()
    }

    fn source(slot: u32) -> Vec<f32> {
        (0..COUNT).map(|i| (slot + 1) as f32 + i as f32).collect()
    }

    /// `route byte = gain << 6 | source << 3 | dest`.
    fn byte(gain: u8, source: u8, dest: u8) -> u8 {
        (gain << 6) | (source << 3) | dest
    }

    #[test]
    fn the_byte_fields_decode_where_the_note_says() {
        // Every field, over every byte value — the three `rlwinm`s produce byte offsets, and getting
        // one shift wrong picks a neighbouring slot that still exists.
        for raw in 0..=255u8 {
            assert_eq!(dest_slot(raw), u32::from(raw) & 7, "dest of {raw:#04X}");
            assert_eq!(source_slot(raw), (u32::from(raw) >> 3) & 7, "source of {raw:#04X}");
            assert_eq!(gain_slot(raw), u32::from(raw) >> 6, "gain of {raw:#04X}");
        }
        // And the packing this test file uses agrees with them.
        for (gain, src, dst) in [(0u8, 0u8, 0u8), (3, 7, 7), (1, 2, 3), (2, 5, 6)] {
            let raw = byte(gain, src, dst);
            assert_eq!((gain_slot(raw), source_slot(raw), dest_slot(raw)),
                       (u32::from(gain), u32::from(src), u32::from(dst)));
        }
    }

    #[test]
    fn one_route_scales_its_source_into_its_destination() {
        let mut g = guest();
        route(&mut g, &[byte(1, 2, 3)]); // gain 2.0, source slot 2, destination slot 3
        run(&mut g, 0); // no closing clear, so only the route's own write is visible

        let want: Vec<f32> = source(2).iter().map(|v| v * 2.0).collect();
        assert_eq!(out(&g, 3), want);
        // Nothing else moved.
        assert_eq!(g.u32(BUFFERS).unwrap(), POISON, "destination 0 untouched");
    }

    #[test]
    fn the_first_route_to_a_destination_overwrites_and_the_rest_accumulate() {
        // The whole point of the flag array. Three routes into slot 1 from three different sources:
        // the answer is the first one's product *replacing* the poison, then two accumulations.
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 1), byte(1, 1, 1), byte(2, 2, 1)]);
        run(&mut g, 0);

        let want: Vec<f32> = (0..COUNT as usize)
            .map(|i| source(0)[i] * 1.0 + source(1)[i] * 2.0 + source(2)[i] * 4.0)
            .collect();
        assert_eq!(out(&g, 1), want);

        // If the first call accumulated instead of overwriting, the poison would still be in there.
        let poisoned = f32::from_bits(POISON);
        assert!(want.iter().all(|v| v.is_finite()));
        assert!(!out(&g, 1).iter().any(|v| *v == poisoned), "the first route must overwrite");
    }

    #[test]
    fn every_untouched_slot_the_caller_owns_is_zeroed() {
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 2)]); // only destination 2 is written
        run(&mut g, 4); // the caller owns four slots

        assert_eq!(out(&g, 2), source(0), "gain 1.0, so the copy is exact");
        for slot in [0u32, 1, 3] {
            assert_eq!(out(&g, slot), vec![0.0f32; COUNT as usize], "slot {slot} cleared");
        }
    }

    #[test]
    fn a_slot_count_of_zero_clears_nothing() {
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 0)]);
        run(&mut g, 0);
        // Slot 1 keeps its poison: the closing loop never ran.
        assert_eq!(g.u32(BUFFERS + STRIDE).unwrap(), POISON);
    }

    #[test]
    fn the_gain_comes_from_the_table_and_is_read_live() {
        for gain_index in 0..4u8 {
            let mut g = guest();
            route(&mut g, &[byte(gain_index, 0, 0)]);
            run(&mut g, 0);
            let want: Vec<f32> = source(0).iter().map(|v| v * GAINS[gain_index as usize]).collect();
            assert_eq!(out(&g, 0), want, "gain index {gain_index}");
        }
        // Patching the table moves the result, so the four floats are loaded rather than assumed.
        let mut g = guest();
        g.set_u32(GAIN_TABLE + 8, 100.0f32.to_bits()).unwrap();
        route(&mut g, &[byte(2, 0, 0)]);
        run(&mut g, 0);
        assert_eq!(out(&g, 0), source(0).iter().map(|v| v * 100.0).collect::<Vec<_>>());
    }

    #[test]
    fn the_range_is_inclusive_and_an_empty_range_runs_no_route() {
        // last < first: the do-while's guard is checked before the first iteration, so nothing runs
        // — but the closing clear still does.
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 0), byte(0, 1, 1)]);
        g.set_u8(RANGE + RANGE_FIRST, 1).unwrap();
        g.set_u8(RANGE + RANGE_LAST, 0).unwrap();
        run(&mut g, 2);
        assert_eq!(out(&g, 0), vec![0.0f32; COUNT as usize], "no route ran, so it was cleared");
        assert_eq!(out(&g, 1), vec![0.0f32; COUNT as usize]);

        // And a range of [1, 1] runs exactly the second route.
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 0), byte(0, 1, 1)]);
        g.set_u8(RANGE + RANGE_FIRST, 1).unwrap();
        g.set_u8(RANGE + RANGE_LAST, 1).unwrap();
        run(&mut g, 0);
        assert_eq!(out(&g, 1), source(1));
        assert_eq!(g.u32(BUFFERS).unwrap(), POISON, "the first route did not run");
    }

    #[test]
    fn the_last_index_is_re_read_every_iteration() {
        // The original reloads `range + 1` each time round, so a route whose destination covers that
        // byte changes its own trip count. Reproduced, and this is what pins it: the destination of
        // route 0 is aimed at the range itself, and the value it writes there shortens the loop.
        let mut g = guest();
        g.set_u32(DEST_ARRAY, RANGE).unwrap(); // destination slot 0 *is* the range word
        route(&mut g, &[byte(0, 0, 0), byte(0, 1, 1)]);
        g.set_u8(RANGE + RANGE_FIRST, 0).unwrap();
        g.set_u8(RANGE + RANGE_LAST, 1).unwrap();
        // Source slot 0's first float is 1.0, whose big-endian bytes are 3F 80 00 00, so after the
        // first route `range[1]` is 0x80 -- larger, not smaller, so the loop would run on. Make the
        // source write zeroes instead, which sets range[1] = 0 and ends the loop after route 0.
        for i in 0..COUNT {
            g.set_u32(BUFFERS + 4 * STRIDE + i * 4, 0).unwrap();
        }

        run(&mut g, 0);

        assert_eq!(g.u8(RANGE + RANGE_LAST).unwrap(), 0, "the range was overwritten");
        // Route 1 never ran: destination slot 1 still holds its poison.
        assert_eq!(g.u32(BUFFERS + STRIDE).unwrap(), POISON);
    }

    #[test]
    fn more_slots_than_flags_is_refused_before_anything_is_written() {
        let mut g = guest();
        route(&mut g, &[byte(0, 0, 0)]);
        let e = scatter_mix(&mut g, DEST_ARRAY, SOURCE_ARRAY, 9, COUNT, RANGE, TABLE);
        assert!(e.is_err(), "nine slots reads a flag byte the original never wrote");
        assert_eq!(g.u32(BUFFERS).unwrap(), POISON, "and nothing ran");
    }
}
