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
//! | [`set_field_364`] | `sub_82B29278` | 11 | 1,684 | 1,685 | a direct `bl` |
//! | [`five_point_ramp`] | `sub_82B2FE00` | 113 | 8 | 8 | a direct `bl` — **thin** |
//! | [`stream_remaining`] | `sub_82B23C10` | 47 | 122,997 | 214,853 | five direct callers |
//! | [`pair_record_size`] | `sub_82B4F8A8` | 17 | 1,930 | 2,166 | a direct `bl` |
//! | [`publish_float`] | `sub_82B49268` | 17 | 745 | 981 | a command-ring record |
//! | [`zero_two_fields`] | `sub_82B3D578` | 18 | 10 | 11 | a direct `bl` — **thin** |
//! | [`copy_and_mark_filled`] | `sub_82B1D840` | 40 | 247 | 343 | a direct `bl` |
//! | [`push_node`] | `sub_82B34B10` | 36 | 284 | 317 | a command-ring record |
//! | [`publish_command`] | `sub_82B23828` | 77 | 1,910 | 1,495 | a command-ring record |
//!
//! **Replayed against the game, 2026-09-13: 6,661 recorded vectors, 0 disagreements**
//! (3,232 + 230 + 230 + 2,969, sessions `leaves` and `leaves2`). The four were recorded on purpose
//! — no earlier capture contained them — by naming them in `AUDIO_VECTORS_ONLY`, and the same
//! sessions compared them live against the original 672,366 / 161,603 / 99,945 / 97,943 times with
//! zero divergence. One limit: the recording keeps only the low word of `r3`, so
//! [`stream_remaining`]'s borrow into the upper word is checked by the tests below and not by any
//! vector.
//!
//! The six added later — [`pair_record_size`] through [`publish_command`] — replay **6,596 recorded
//! calls, 0 disagreements** (session `leaves3`, 2,619 / 1,404 / 11 / 924 / 397 / 1,241); the thin
//! [`zero_two_fields`] carries only eleven, which is all a session produces.
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

/// `sub_82B29278`'s only store. The same shape as [`set_field_460`], at `+364` (`0x16C`); nothing
/// names either offset.
pub const FIELD_364: u32 = 364;

/// Store `value` as a `u16` at `object + 364`, and return 0 (`sub_82B29278`) — [`set_field_460`]'s
/// twin: `sth r6,364(r11)` and `li r3,0`, with the value in the guest's fourth argument.
pub fn set_field_364(g: &mut Guest, object: u32, value: u16) -> Result<u64> {
    g.set_u16(object + FIELD_364, value)?; // sth r6,364(r11)
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

// --------------------------------------------------------------- more small verified leaves

/// `li r9,16` — the alignment [`pair_record_size`] reports through `r4`.
pub const PAIR_RECORD_ALIGNMENT: u32 = 16;
/// `addi r3,r11,88` — the record's fixed header.
pub const PAIR_RECORD_HEADER: u64 = 88;
/// `mulli r11,r10,28` — bytes per pair of entries.
pub const PAIR_RECORD_STRIDE: u64 = 28;

/// The size of a record holding `count` entries two to a slot, after an 88-byte header
/// (`sub_82B4F8A8`). Stores the alignment, 16, through `out`, and returns `28 * ceil(count/2) + 88`.
///
/// The halving is `addi` then a 32-bit logical shift right, so a count of `0xFFFF_FFFF` wraps to zero
/// slots rather than to 2^31.
pub fn pair_record_size(g: &mut Guest, count: u32, out: u32) -> Result<u64> {
    let pairs = count.wrapping_add(1) >> 1; // addi r11,r3,1 ; rlwinm r10,r11,31,1,31
    g.set_u32(out, PAIR_RECORD_ALIGNMENT)?; // stw r9,0(r4)
    Ok(u64::from(pairs) * PAIR_RECORD_STRIDE + PAIR_RECORD_HEADER) // mulli ; addi
}

/// `lis -32234 ; lfs 23056` — the image's zero single, the cell [`crate::mix::ZERO_SINGLE`] names.
pub const ZERO_CELL: u32 = (((-32234i32 as u32) & 0xFFFF) << 16).wrapping_add(23056);
const _: () = assert!(ZERO_CELL == 0x8216_5A10, "lis -32234 ; lfs 23056");

/// A publishing command record: `{+0 handler, +4 target, +8 value …}`.
pub const COMMAND_TARGET: u32 = 4;

/// Publish the single at `record + 8` onto `target + 56`, and return the record size, 12
/// (`sub_82B49268`). A command-ring handler, like [`stamp_slot`].
#[cfg(target_arch = "x86_64")]
pub fn publish_float(g: &mut Guest, record: u32) -> Result<u64> {
    let target = g.u32(record + COMMAND_TARGET)?; // lwz r11,4(r3)
    let mut fpscr = crate::vmx::Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // emitted before the lfs
    let value = fp::load_single(g, record + 8)?; // lfs f0,8(r3)
    fp::store_single(g, target.wrapping_add(56), value)?; // stfs f0,56(r11)
    drop(fpscr);
    Ok(12) // li r3,12
}

/// Store the image's zero into the two singles at `+16` and `+20` (`sub_82B3D578`, **thin**: ten
/// calls a session). The cell is read live rather than assumed to be `0.0`.
#[cfg(target_arch = "x86_64")]
pub fn zero_two_fields(g: &mut Guest, object: u32) -> Result<()> {
    let mut fpscr = crate::vmx::Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();
    let zero = fp::load_single(g, ZERO_CELL)?; // lfs f0,23056(r11)
    fp::store_single(g, object.wrapping_add(16), zero)?; // stfs f0,16(r3)
    fp::store_single(g, object.wrapping_add(20), zero) // stfs f0,20(r3)
}

/// `lbz r10,24(r4)` — the destination's element count, re-read after every copied word.
pub const FILL_COUNT: u32 = 24;
/// `stb r11,25(r4)` — set to 1 on the way out, whether or not anything was copied.
pub const FILL_FLAG: u32 = 25;
/// Where the first copied word lands: `stwu 4(r9)` with `r9 = dest + 24`.
pub const FILL_FIRST_WORD: u32 = 28;

/// Copy `count` words from `source` into the destination block and mark it filled (`sub_82B1D840`).
///
/// A do-while: with a non-zero count the first word is copied before the count is tested, and the
/// count byte is **re-read after every word**. No store here can reach it — the copy ascends from
/// `+28` — so the reload is unobservable in practice, and it is kept because the original has it.
pub fn copy_and_mark_filled(g: &mut Guest, source: u32, dest: u32) -> Result<()> {
    if g.u8(dest + FILL_COUNT)? != 0 {
        let (mut from, mut to) = (source, dest.wrapping_add(FILL_FIRST_WORD));
        let mut copied = 0u32;
        loop {
            let word = g.u32(from)?; // lwzu r8,4(r10)
            g.set_u32(to, word)?; // stwu r8,4(r9)
            copied += 1;
            let count = g.u8(dest + FILL_COUNT)?; // lbz r8,24(r4) -- reloaded
            if copied as i32 >= i32::from(count) {
                break;
            }
            from = from.wrapping_add(4);
            to = to.wrapping_add(4);
        }
    }
    g.set_u8(dest + FILL_FLAG, 1) // li r11,1 ; stb r11,25(r4)
}

/// `lis -31988 ; lwz r9,-8520(r10)` — the head of a global singly-linked list.
pub const LIST_HEAD: u32 = (((-31988i32 as u32) & 0xFFFF) << 16).wrapping_sub(8520);
const _: () = assert!(LIST_HEAD == 0x830B_DEB8, "lis -31988 ; -8520");
/// The node on the object: `+44` next, `+48` a second word cleared on push.
pub const NODE_OFFSET: u32 = 44;
/// `stb r9,164(r8)` — set once the object is on the list.
pub const NODE_LINKED: u32 = 164;

/// Push the record's object onto the global list and return the record size, 8 (`sub_82B34B10`).
///
/// The head is loaded, the node's two words are written, and then the head is **loaded again** before
/// the previous head's back-link is set — the compiler could not prove the two stores miss the head
/// cell. Reproduced; the C++ window builder refuses the one layout where it matters.
pub fn push_node(g: &mut Guest, record: u32) -> Result<u64> {
    let object = g.u32(record + COMMAND_TARGET)?; // lwz r8,4(r3)
    let node = object.wrapping_add(NODE_OFFSET); // addi r11,r8,44
    let head = g.u32(LIST_HEAD)?; // lwz r9,-8520(r10)
    g.set_u32(object.wrapping_add(48), 0)?; // stw r7,48(r8)
    g.set_u32(node, head)?; // stw r9,44(r8)
    let head_again = g.u32(LIST_HEAD)?; // lwz r9,-8520(r10) -- reloaded
    if head_again != 0 {
        g.set_u32(head_again.wrapping_add(4), node)?; // stw r11,4(r9)
    }
    g.set_u32(LIST_HEAD, node)?; // stw r11,-8520(r10)
    g.set_u8(object.wrapping_add(NODE_LINKED), 1)?; // stb r9,164(r8)
    Ok(8) // li r3,8
}

/// `lis -32208 ; addi r8,r9,-31232 ; lfd f13,256(r8)` — the double a command's value is tested
/// against to choose the "clear" form.
pub const SENTINEL_DOUBLE: u32 = (((-32208i32 as u32) & 0xFFFF) << 16).wrapping_sub(31232) + 256;
const _: () = assert!(SENTINEL_DOUBLE == 0x822F_8700, "lis -32208 ; addi -31232 ; lfd 256");

/// Publish a command's value onto its target in one of two forms, returning the record size, 32
/// (`sub_82B23828`).
///
/// The record is `{+4 target, +8 double, +16 single A, +20 single B, +24 word}`. When the double
/// equals the image's sentinel **and** single A equals the image's zero, the "clear" form stores B at
/// `+108` and zeroes the flag bytes at `+113` and `+112`. Otherwise the "set" form stores the double
/// at `+56`, A at `+64`, B at `+68`, the word at `+72`, and sets `+112`. The double comparison is
/// unordered, so a NaN takes the set form; and single A is only read when the double matched.
#[cfg(target_arch = "x86_64")]
pub fn publish_command(g: &mut Guest, record: u32) -> Result<u64> {
    let mut fpscr = crate::vmx::Fpscr::capture();
    fpscr.disable_flush_mode_unconditional(); // emitted before the lfd
    let bits = g.u64(record + 8)?; // lfd f0,8(r3)
    let target = g.u32(record + COMMAND_TARGET)?; // lwz r11,4(r3)
    let sentinel = fp::load_double(g, SENTINEL_DOUBLE)?; // lfd f13,256(r8)

    if f64::from_bits(bits) == sentinel
        && fp::load_single(g, record + 16)? == fp::load_single(g, ZERO_CELL)?
    {
        let b = fp::load_single(g, record + 20)?;
        fp::store_single(g, target.wrapping_add(108), b)?; // stfs f0,108(r11)
        g.set_u8(target.wrapping_add(113), 0)?; // stb r9,113(r11)
        g.set_u8(target.wrapping_add(112), 0)?; // stb r9,112(r11)
        return Ok(32);
    }
    g.set_u64(target.wrapping_add(56), bits)?; // stfd f0,56(r11)
    let a = fp::load_single(g, record + 16)?;
    fp::store_single(g, target.wrapping_add(64), a)?; // stfs f0,64(r11)
    let b = fp::load_single(g, record + 20)?;
    fp::store_single(g, target.wrapping_add(68), b)?; // stfs f13,68(r11)
    let word = g.u32(record + 24)?;
    g.set_u32(target.wrapping_add(72), word)?; // stw r8,72(r11)
    g.set_u8(target.wrapping_add(112), 1)?; // stb r9,112(r11)
    Ok(32) // li r3,32
}

// ------------------------------------------------------------------ the five-point ramp (thin)

/// `lis -32208 ; addi -31232` — the pool three of the ramp's constants sit in.
const RAMP_POOL: u32 = (((-32208i32 as u32) & 0xFFFF) << 16).wrapping_sub(31232);
/// `lfs f13,468(r11)` — the rate's upper limit.
pub const RAMP_UPPER_LIMIT: u32 = RAMP_POOL + 468;
/// `lfs f0,472(r11)` — where the ramp starts when the span hits the ceiling.
pub const RAMP_BASE: u32 = RAMP_POOL + 472;
/// `lfs f12,476(r11)` — the rate written back when the span hits the ceiling.
pub const RAMP_START: u32 = RAMP_POOL + 476;
/// `lis -32250 ; lfs 3152` — the rate's lower limit.
pub const RAMP_LOWER_LIMIT: u32 = (((-32250i32 as u32) & 0xFFFF) << 16) + 3152;
/// `lis -32247 ; lfs -32180` — the rate-to-first-point scale.
pub const RAMP_SCALE_A: u32 = (((-32247i32 as u32) & 0xFFFF) << 16).wrapping_sub(32180);
/// `lis -32222 ; lfs 18868` — the first-point-to-span scale. **0x49B4, not 0x4BB4**: reading this
/// immediate by eye produced the project's first shadow divergence, which is why it is computed.
pub const RAMP_SCALE_B: u32 = (((-32222i32 as u32) & 0xFFFF) << 16) + 18868;
/// `lis -32241 ; lfs -10884` — the span's ceiling (the clipper's 100.0 cell).
pub const RAMP_CEILING: u32 = (((-32241i32 as u32) & 0xFFFF) << 16).wrapping_sub(10884);
/// `lis -32246 ; lfs -28032` — the step scale.
pub const RAMP_STEP_SCALE: u32 = (((-32246i32 as u32) & 0xFFFF) << 16).wrapping_sub(28032);
const _: () = assert!(RAMP_POOL == 0x822F_8600 && RAMP_UPPER_LIMIT == 0x822F_87D4);
const _: () = assert!(RAMP_SCALE_A == 0x8208_824C && RAMP_SCALE_B == 0x8222_49B4);
const _: () = assert!(RAMP_CEILING == 0x820E_D57C && RAMP_STEP_SCALE == 0x8209_9280);
const _: () = assert!(RAMP_LOWER_LIMIT == 0x8206_0C50);

/// Clamp a rate into range, then fill a five-point ramp plus its span (`sub_82B2FE00`, **thin**:
/// eight calls a session). `rate_field` is `r3`, `out` is `r4`; returns 1.
///
/// The rate is clamped in place — above the upper limit to it, and below the lower limit **or NaN**
/// to the lower limit, since the second compare is `bge` and an unordered rate fails it. Then the rate
/// is **re-read** from memory, scaled into a first point and a span, and if the span passes the
/// ceiling the ramp restarts from the pool's own base and the rate field is overwritten a second
/// time. The five points are an arithmetic progression from `first`, each `fadds` single-rounded from
/// the one before rather than computed as `first + k*step`, and the span is stored at `+20` before the
/// points are.
#[cfg(target_arch = "x86_64")]
pub fn five_point_ramp(g: &mut Guest, rate_field: u32, out: u32) -> Result<u64> {
    let mut fpscr = crate::vmx::Fpscr::capture();
    fpscr.disable_flush_mode_unconditional();
    let rate = fp::load_single(g, rate_field)?; // lfs f0,0(r3)
    let mut limit = fp::load_single(g, RAMP_UPPER_LIMIT)?; // lfs f13,468(r11)
    let mut clamp = rate > limit; // fcmpu ; bgt
    if !clamp {
        limit = fp::load_single(g, RAMP_LOWER_LIMIT)?; // lfs f13,3152(r10)
        clamp = !(rate >= limit); // bge -- so a NaN clamps, to the lower limit
    }
    if clamp {
        fp::store_single(g, rate_field, limit)?; // stfs f13,0(r3)
    }
    let clamped = fp::load_single(g, rate_field)?; // lfs f13,0(r3) -- reloaded
    let mut first = fp::mul_single(clamped, fp::load_single(g, RAMP_SCALE_A)?); // fmuls f0,f13,f0
    let mut span = fp::mul_single(first, fp::load_single(g, RAMP_SCALE_B)?); // fmuls f13,f0,f13
    let ceiling = fp::load_single(g, RAMP_CEILING)?; // lfs f12,-10884(r8)
    if span > ceiling {
        span = ceiling; // fmr f13,f12
        let start = fp::load_single(g, RAMP_START)?; // lfs f12,476(r11)
        first = fp::load_single(g, RAMP_BASE)?; // lfs f0,472(r11)
        fp::store_single(g, rate_field, start)?; // stfs f12,0(r3)
    }
    let width = fp::sub_single(span, first); // fsubs f12,f13,f0
    fp::store_single(g, out.wrapping_add(20), span)?; // stfs f13,20(r4)
    fp::store_single(g, out, first)?; // stfs f0,0(r4)
    let step = fp::mul_single(width, fp::load_single(g, RAMP_STEP_SCALE)?); // fmuls f11,f12,f13
    let mut point = first;
    for k in 1..5u32 {
        point = fp::add_single(if k == 1 { step } else { point }, if k == 1 { first } else { step });
        fp::store_single(g, out.wrapping_add(4 * k), point)?; // fadds ; stfs +4, +8, +12, +16
    }
    Ok(1) // li r3,1
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

    // ------------------------------------------------------------ more small verified leaves

    const L2: u32 = 0x5000_0000;

    fn small() -> Guest {
        let mut g = Guest::single(L2, 0x1000);
        g.put(ZERO_CELL, 0.0f32.to_bits().to_be_bytes().to_vec());
        g.put(LIST_HEAD, vec![0u8; 4]);
        g.put(SENTINEL_DOUBLE, (-1.0f64).to_bits().to_be_bytes().to_vec());
        g
    }

    #[test]
    fn the_pair_record_size_rounds_the_count_up_to_pairs() {
        let mut g = small();
        for (count, want) in [(0u32, 88u64), (1, 116), (2, 116), (3, 144), (10, 228)] {
            assert_eq!(pair_record_size(&mut g, count, L2).unwrap(), want, "count {count}");
            assert_eq!(g.u32(L2).unwrap(), 16, "the alignment, stored through r4");
        }
        // addi then a 32-bit shift: 0xFFFFFFFF + 1 wraps to zero pairs.
        assert_eq!(pair_record_size(&mut g, u32::MAX, L2).unwrap(), 88);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn publish_float_lands_on_the_target_and_returns_twelve() {
        let mut g = small();
        let (record, target) = (L2, L2 + 0x100);
        g.set_u32(record + COMMAND_TARGET, target).unwrap();
        g.set_u32(record + 8, 0.625f32.to_bits()).unwrap();
        assert_eq!(publish_float(&mut g, record).unwrap(), 12);
        assert_eq!(g.f32(target + 56).unwrap(), 0.625);
        assert_eq!(g.u32(target + 60).unwrap(), 0, "one word, not two");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_two_fields_take_the_image_zero_read_live() {
        let mut g = small();
        g.set_u32(ZERO_CELL, 2.5f32.to_bits()).unwrap(); // patch the cell: it is read, not assumed
        g.set_u32(L2 + 12, 0xDEAD_BEEF).unwrap();
        g.set_u32(L2 + 24, 0xDEAD_BEEF).unwrap();
        zero_two_fields(&mut g, L2).unwrap();
        assert_eq!((g.f32(L2 + 16).unwrap(), g.f32(L2 + 20).unwrap()), (2.5, 2.5));
        assert_eq!((g.u32(L2 + 12).unwrap(), g.u32(L2 + 24).unwrap()), (0xDEAD_BEEF, 0xDEAD_BEEF));
    }

    #[test]
    fn copy_and_mark_filled_copies_count_words_then_sets_the_flag() {
        let mut g = small();
        let (source, dest) = (L2, L2 + 0x100);
        for i in 0..8u32 {
            g.set_u32(source + 4 * i, 0x1000 + i).unwrap();
            g.set_u32(dest + FILL_FIRST_WORD + 4 * i, 0xDEAD_BEEF).unwrap();
        }
        g.set_u8(dest + FILL_COUNT, 3).unwrap();
        copy_and_mark_filled(&mut g, source, dest).unwrap();
        for i in 0..3u32 {
            assert_eq!(g.u32(dest + FILL_FIRST_WORD + 4 * i).unwrap(), 0x1000 + i);
        }
        assert_eq!(g.u32(dest + FILL_FIRST_WORD + 12).unwrap(), 0xDEAD_BEEF, "three words");
        assert_eq!(g.u8(dest + FILL_FLAG).unwrap(), 1);

        // A zero count copies nothing but still marks the block.
        let mut g = small();
        g.set_u32(dest + FILL_FIRST_WORD, 0xDEAD_BEEF).unwrap();
        copy_and_mark_filled(&mut g, source, dest).unwrap();
        assert_eq!(g.u32(dest + FILL_FIRST_WORD).unwrap(), 0xDEAD_BEEF);
        assert_eq!(g.u8(dest + FILL_FLAG).unwrap(), 1);
    }

    #[test]
    fn push_node_onto_an_empty_then_a_non_empty_list() {
        let mut g = small();
        let (record, first, second) = (L2, L2 + 0x200, L2 + 0x400);
        g.set_u32(record + COMMAND_TARGET, first).unwrap();
        g.set_u32(first + 48, 0xDEAD_BEEF).unwrap();
        assert_eq!(push_node(&mut g, record).unwrap(), 8);
        assert_eq!(g.u32(LIST_HEAD).unwrap(), first + NODE_OFFSET, "the head is the node, not the object");
        assert_eq!(g.u32(first + NODE_OFFSET).unwrap(), 0, "an empty list's next is null");
        assert_eq!(g.u32(first + 48).unwrap(), 0, "and the second word is cleared");
        assert_eq!(g.u8(first + NODE_LINKED).unwrap(), 1);

        g.set_u32(record + COMMAND_TARGET, second).unwrap();
        push_node(&mut g, record).unwrap();
        assert_eq!(g.u32(LIST_HEAD).unwrap(), second + NODE_OFFSET);
        assert_eq!(g.u32(second + NODE_OFFSET).unwrap(), first + NODE_OFFSET, "next is the old head");
        assert_eq!(g.u32(first + NODE_OFFSET + 4).unwrap(), second + NODE_OFFSET, "the old head's back-link");
    }

    #[cfg(target_arch = "x86_64")]
    fn command(g: &mut Guest, value: f64, a: f32, b: f32, word: u32) -> (u32, u32) {
        let (record, target) = (L2, L2 + 0x100);
        g.set_u32(record + COMMAND_TARGET, target).unwrap();
        g.set_u64(record + 8, value.to_bits()).unwrap();
        g.set_u32(record + 16, a.to_bits()).unwrap();
        g.set_u32(record + 20, b.to_bits()).unwrap();
        g.set_u32(record + 24, word).unwrap();
        for off in (56..116).step_by(4) {
            g.set_u32(target + off, 0xDEAD_BEEF).unwrap();
        }
        (record, target)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_command_publishes_the_set_form_unless_it_is_the_clear_one() {
        // Not the sentinel: the set form.
        let mut g = small();
        let (record, target) = command(&mut g, 3.5, 1.25, 2.25, 77);
        assert_eq!(publish_command(&mut g, record).unwrap(), 32);
        assert_eq!(g.u64(target + 56).unwrap(), 3.5f64.to_bits());
        assert_eq!((g.f32(target + 64).unwrap(), g.f32(target + 68).unwrap()), (1.25, 2.25));
        assert_eq!(g.u32(target + 72).unwrap(), 77);
        assert_eq!(g.u8(target + 112).unwrap(), 1);
        assert_eq!(g.u32(target + 108).unwrap(), 0xDEAD_BEEF, "+108 is the clear form's field");

        // The sentinel and a zero A: the clear form.
        let mut g = small();
        let (record, target) = command(&mut g, -1.0, 0.0, 2.25, 77);
        assert_eq!(publish_command(&mut g, record).unwrap(), 32);
        assert_eq!(g.f32(target + 108).unwrap(), 2.25);
        assert_eq!((g.u8(target + 112).unwrap(), g.u8(target + 113).unwrap()), (0, 0));
        assert_eq!(g.u32(target + 56).unwrap(), 0xDEAD_BEEF, "the set form's fields are untouched");

        // The sentinel but a non-zero A: still the set form — both halves of the test are needed.
        let mut g = small();
        let (record, target) = command(&mut g, -1.0, 0.5, 2.25, 77);
        publish_command(&mut g, record).unwrap();
        assert_eq!(g.u8(target + 112).unwrap(), 1);

        // A NaN value is unordered, never equal to the sentinel: the set form.
        let mut g = small();
        let (record, target) = command(&mut g, f64::NAN, 0.0, 2.25, 77);
        publish_command(&mut g, record).unwrap();
        assert_eq!(g.u8(target + 112).unwrap(), 1);
    }

    #[test]
    fn the_new_leaf_addresses_come_from_the_lis_immediates() {
        assert_eq!(LIST_HEAD, 0x830C_0000 - 8520);
        assert_eq!(SENTINEL_DOUBLE, 0x8230_0000 - 31232 + 256);
        assert_eq!(ZERO_CELL, crate::mix::ZERO_SINGLE, "the image's zero, reached again");
    }

    #[test]
    fn the_field_at_364_is_two_bytes_and_the_result_is_zero() {
        let mut g = Guest::single(BASE, 0x400);
        g.set_u32(BASE + FIELD_364, POISON).unwrap();
        assert_eq!(set_field_364(&mut g, BASE, 0xABCD).unwrap(), 0);
        assert_eq!(g.u16(BASE + FIELD_364).unwrap(), 0xABCD);
        assert_eq!(g.u16(BASE + FIELD_364 + 2).unwrap(), 0xBEEF, "two bytes, not four");
    }

    #[cfg(target_arch = "x86_64")]
    fn ramp_guest(rate: f32) -> Guest {
        let mut g = Guest::single(BASE, 0x400);
        for (addr, v) in [
            (RAMP_UPPER_LIMIT, 48000.0f32), (RAMP_BASE, 3.0), (RAMP_START, 44100.0),
            (RAMP_LOWER_LIMIT, 2.0), (RAMP_SCALE_A, 0.001), (RAMP_SCALE_B, 4.0),
            (RAMP_CEILING, 100.0), (RAMP_STEP_SCALE, 0.25),
        ] {
            g.put(addr, v.to_bits().to_be_bytes().to_vec());
        }
        g.set_u32(BASE, rate.to_bits()).unwrap();
        g
    }

    #[cfg(target_arch = "x86_64")]
    fn ramp_points(g: &Guest) -> Vec<f32> {
        (0..6u32).map(|k| g.f32(BASE + 0x40 + 4 * k).unwrap()).collect()
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_ramp_is_an_arithmetic_progression_from_the_scaled_rate() {
        let mut g = ramp_guest(10_000.0);
        assert_eq!(five_point_ramp(&mut g, BASE, BASE + 0x40).unwrap(), 1);
        let first = 10_000.0f32 * 0.001; // 10
        let span = first * 4.0; // 40, under the ceiling
        let step = (span - first) * 0.25; // 7.5
        assert_eq!(ramp_points(&g), vec![first, first + step, first + 2.0 * step, first + 3.0 * step, first + 4.0 * step, span]);
        assert_eq!(g.f32(BASE).unwrap(), 10_000.0, "an in-range rate is left alone");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_rate_clamps_both_ways_and_a_nan_takes_the_lower_limit() {
        for (rate, want) in [(90_000.0f32, 48_000.0f32), (1.0, 2.0), (f32::NAN, 2.0)] {
            let mut g = ramp_guest(rate);
            five_point_ramp(&mut g, BASE, BASE + 0x40).unwrap();
            // 48000 * 0.001 * 4 = 192 is past the ceiling, so the upper clamp is then overwritten by
            // the pool's start value; the other two stay at the lower limit.
            let stored = g.f32(BASE).unwrap();
            if rate > 48_000.0 {
                assert_eq!(stored, 44_100.0, "rate {rate}: the ceiling path rewrites it");
            } else {
                assert_eq!(stored, want, "rate {rate}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_span_past_the_ceiling_restarts_from_the_pool_base() {
        let mut g = ramp_guest(40_000.0); // first 40, span 160 > 100
        five_point_ramp(&mut g, BASE, BASE + 0x40).unwrap();
        let pts = ramp_points(&g);
        assert_eq!(pts[0], 3.0, "the ramp starts from the pool's base");
        assert_eq!(pts[5], 100.0, "and spans to the ceiling");
        assert_eq!(pts[1], 3.0 + (100.0 - 3.0) * 0.25);
        assert_eq!(g.f32(BASE).unwrap(), 44_100.0, "the rate field is overwritten a second time");
    }
}
