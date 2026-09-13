//! The packet stream's decoders: a bit reader, two variable-length integer codes, the run-length
//! layer over them, and the paged bit cursor.
//!
//! Seven verified leaves and near-leaves, ported from their `recomp/src/audio_ports/sub_*.inc`. None
//! of them touches a float; they are integer work over big-endian bytes, which is why this module is
//! not gated on x86 the way the mixer's are.
//!
//! | function | guest | lifted lines | calls/boot | calls/play |
//! |---|---|---|---|---|
//! | [`read_bits`] | `sub_82B26B70` | 64 | 36,190 | 42,524 |
//! | [`decode_unsigned`] | `sub_82B4F5F8` | 127 | 32,974 | 22,336 |
//! | [`decode_signed`] | `sub_82B46EA0` | 150 | 32,604 | 26,780 |
//! | [`decode_signed_into`] | `sub_82B46FB0` | 53 | 15,048 | 12,360 |
//! | [`run_length_step`] | `sub_82B47010` | 107 | 12,540 | 10,300 |
//! | [`decode_four`] | `sub_82B47658` | 60 | 2,508 | 2,060 |
//! | [`advance_bit_cursor`] | `sub_82B50270` | 162 | 6,354 | 5,518 |
//! | [`unpack_stream_header`] | `sub_82B31D90` | 93 | 745 | 981 |
//!
//! Replayed against **20,000 recorded calls, 0 disagreements**, and each compared live against the
//! original between 481 and 17,668 times in the same session.
//!
//! ## The two variable-length codes
//!
//! Both read a prefix byte whose *value* picks the width — below 192 one byte, below 240 two, below
//! 252 three, below 255 four, and 255 means four raw bytes follow. The payload is what the prefix
//! leaves over, plus a bias that makes each width start exactly where the previous one ended, so no
//! value has two short encodings.
//!
//! The signed code is **zigzag**: the last byte's low bit is the sign, and a set bit means the value
//! is `!magnitude` (that is, `-1 - magnitude`). One magnitude step costs two encoded steps, so every
//! signed bias is exactly half the unsigned one — 96, 6240 and 399,456 against 192, 12,480 and
//! 798,912. The five-byte form of either code is the raw 32-bit word, with no zigzag and no bias.
//!
//! The tests encode with an encoder written from that description and check the decoders round-trip
//! it across every width boundary, which is what catches a mask one bit off.
//!
//! ## The run-length layer
//!
//! A sub-object `{cursor* +0, accumulated +4, remaining +8, flag +12}` walks a stream of signed codes.
//! When its run is exhausted it reads a **count** code: a non-negative count `c` starts a *repeat*
//! run — one delta is read now, and the running sum is returned `c + 1` times — and a negative count
//! starts a *literal* run of `1 - c` values, each with its own delta. [`run_length_step`] returns the
//! running sum on every call. [`decode_four`] runs four of these side by side.
//!
//! ## The paged bit cursor
//!
//! [`advance_bit_cursor`] reads a 15-bit field at a bit cursor and advances the cursor by it. The
//! stream is paged: 2,048-byte pages whose first 32 bits are a header, so each page carries 16,352
//! payload bits. A field that straddles a page end takes its tail from the next page past that
//! page's header, and a cursor that runs off the end steps the buffer base on by one page.
//!
//! ## What is not reproduced
//!
//! Two of these open a 112-byte frame and hand the callee a scratch word at `sp + 80` to decode into.
//! Here the decoder returns the value instead ([`signed_code`], [`unsigned_code`]), so there is no
//! scratch word and no frame: nothing but this function ever reads that word, and the guest-facing
//! [`decode_signed`] and [`decode_unsigned`] still store exactly where the original does.

use crate::{Guest, Result};

// ------------------------------------------------------------------------ the MSB-first bit reader

/// `lwz r6,0(r3)` — the reader's byte stream.
pub const READER_DATA: u32 = 0;
/// `lwz r10,4(r8)` — the absolute bit position, **reloaded every iteration**.
pub const READER_CURSOR: u32 = 4;

/// Read `bits` bits MSB-first from the reader at `reader`, advancing its cursor (`sub_82B26B70`).
///
/// A do-while over bytes: each pass takes the rest of the current byte or what is left, whichever is
/// smaller. So a request for **zero** bits still loads one byte and stores the cursor back unchanged,
/// which is observable and reproduced. The count is 64-bit in the guest and the loop ends when its
/// low word reaches zero; returns the accumulated value, zero-extended.
pub fn read_bits(g: &mut Guest, reader: u32, bits: u64) -> Result<u64> {
    let data = g.u32(reader + READER_DATA)?; // lwz r6,0(r3)
    let mut remaining = bits; // subf. r4,r11,r4 works in 64 bits
    let mut accumulator = 0u32; // li r3,0
    loop {
        let cursor = g.u32(reader + READER_CURSOR)?; // lwz r10,4(r8)
        let bit_in_byte = cursor & 7; // clrlwi r9,r10,29
        let byte_index = cursor >> 3; // rlwinm r7,r10,29,3,31
        let mut take = 8 - bit_in_byte; // subfic r11,r9,8
        if take > remaining as u32 {
            take = remaining as u32; // cmplw ; mr r11,r4 -- a 32-bit unsigned compare
        }
        let shift = (8 - take - bit_in_byte) & 0xFF;
        let byte = u32::from(g.u8(data.wrapping_add(byte_index))?); // lbzx r7,r6,r7
        accumulator <<= take; // slw r3,r3,r11 -- take is at most 8
        let mask = (1u32 << take) - 1; // slw r9,r5,r11 ; addi r9,r9,-1
        accumulator |= (byte >> shift) & mask;
        g.set_u32(reader + READER_CURSOR, cursor.wrapping_add(take))?; // stw r7,4(r8)
        remaining = remaining.wrapping_sub(u64::from(take));
        if remaining as u32 as i32 == 0 {
            break; // bne on cr0 from the 32-bit signed compare
        }
    }
    Ok(u64::from(accumulator))
}

// ------------------------------------------------------------------- the variable-length codes

/// The prefix thresholds shared by both codes: below each, the code is 1, 2, 3 or 4 bytes long.
pub const PREFIX_LIMITS: [i32; 4] = [192, 240, 252, 255];
/// The unsigned code's bias for its 2-, 3- and 4-byte forms. Each is where the previous width ends.
pub const UNSIGNED_BIASES: [u32; 3] = [192, 12_480, 786_432 + 12_480];
/// The signed code's biases: exactly half the unsigned ones, because the payload is zigzagged.
pub const SIGNED_BIASES: [u32; 3] = [96, 6_240, 393_216 + 6_240];

const _: () = assert!(
    SIGNED_BIASES[0] * 2 == UNSIGNED_BIASES[0]
        && SIGNED_BIASES[1] * 2 == UNSIGNED_BIASES[1]
        && SIGNED_BIASES[2] * 2 == UNSIGNED_BIASES[2],
    "one magnitude step is two encoded steps"
);

/// `rlwinm rX,rY,8,0,23`: rotate left eight and keep bits 0..23. The byte that wraps round is zero
/// on every path that uses it, but the rotate is what the original does.
fn rotate_in_byte(value: u32) -> u32 {
    value.rotate_left(8) & 0xFFFF_FF00
}

/// The bytes of a code, read on demand: the prefix decides how many are touched.
fn code_byte(g: &Guest, stream: u32, index: u32) -> Result<u32> {
    Ok(u32::from(g.u8(stream.wrapping_add(index))?))
}

/// One unsigned variable-length code at `stream`: `(value, bytes consumed)`.
pub fn unsigned_code(g: &Guest, stream: u32) -> Result<(u32, u32)> {
    let b0 = code_byte(g, stream, 0)?; // lbz r10,0(r3)
    // Every compare is cmpwi on the zero-extended byte, so the ladder is on 0..255.
    Ok(if (b0 as i32) < PREFIX_LIMITS[0] {
        (b0, 1)
    } else if (b0 as i32) < PREFIX_LIMITS[1] {
        let b1 = code_byte(g, stream, 1)?;
        // rlwinm r11,r9,0,18,15 clears bits 14 and 15 of the pair: the 0xC0 prefix.
        ((((b0 << 8) | b1) & 0xFFFF_3FFF).wrapping_add(UNSIGNED_BIASES[0]), 2)
    } else if (b0 as i32) < PREFIX_LIMITS[2] {
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        // rlwinm r11,r11,0,12,7 clears bits 20..23: the 0xF0 prefix.
        (((((b0 << 8) | b1) << 8 | b2) & 0xFF0F_FFFF).wrapping_add(UNSIGNED_BIASES[1]), 3)
    } else if (b0 as i32) < PREFIX_LIMITS[3] {
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        let b3 = code_byte(g, stream, 3)?;
        // rlwinm r8,r10,24,6,7 keeps the prefix's low two bits as bits 24..25.
        let high = (b0 & 3) << 24;
        let low = ((((b1 << 8) | b2) << 8) | b3) & 0x3FF_FFFF;
        ((low | high).wrapping_add(UNSIGNED_BIASES[2]), 4)
    } else {
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        let b3 = code_byte(g, stream, 3)?;
        let b4 = code_byte(g, stream, 4)?;
        ((b1 << 24) | (b2 << 16) | (b3 << 8) | b4, 5) // no bias on the widest form
    })
}

/// Decode one unsigned code at `stream`, store it at `out`, return its length (`sub_82B4F5F8`).
pub fn decode_unsigned(g: &mut Guest, stream: u32, out: u32) -> Result<u64> {
    let (value, length) = unsigned_code(g, stream)?;
    g.set_u32(out, value)?; // stw r10,0(r4)
    Ok(u64::from(length)) // li r3,1..5
}

/// One signed zigzag code at `stream`: `(value as a u32, bytes consumed)`.
pub fn signed_code(g: &Guest, stream: u32) -> Result<(u32, u32)> {
    let b0 = code_byte(g, stream, 0)?; // lbz r10,0(r3)
    // (magnitude, the zigzag sign bit, length). The sign is bit 0 of the code's LAST byte.
    let (magnitude, low_bit, length): (u64, u32, u32) = if (b0 as i32) < PREFIX_LIMITS[0] {
        (u64::from(b0 >> 1), b0 & 1, 1) // rlwinm r11,r10,31,1,31 ; clrlwi r10,r10,31
    } else if (b0 as i32) < PREFIX_LIMITS[1] {
        let b1 = code_byte(g, stream, 1)?;
        let pair = ((b0 << 8) & 0xFFFF_FF00) | b1; // rlwinm r10,r10,8,0,23 ; or
        // srawi r8,r9,1 ; rlwinm r11,r8,0,19,16 -- halve, then clear bits 13 and 14, which is where
        // the 0xC0 prefix lands once the pair has been shifted down one.
        let v = (((pair as i32) >> 1) as u32 & 0xFFFF_9FFF).wrapping_add(SIGNED_BIASES[0]);
        (u64::from(v), b1 & 1, 2)
    } else if (b0 as i32) < PREFIX_LIMITS[2] {
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        let pair = ((b0 << 8) & 0xFFFF_FF00) | b1;
        // rlwinm r11,r6,0,20,15 strips the 0xF0 prefix BEFORE the third byte is appended.
        let trio = rotate_in_byte(pair & 0xFFFF_0FFF) | b2;
        (u64::from(((trio as i32) >> 1) as u32) + u64::from(SIGNED_BIASES[1]), b2 & 1, 3)
    } else if (b0 as i32) < PREFIX_LIMITS[3] {
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        let b3 = code_byte(g, stream, 3)?;
        let high = b0.rotate_left(16) & 0x3_0000; // rlwinm r8,r10,16,14,15
        let mid = (b1.rotate_left(8) | b2) & 0x3_FFFF; // clrlwi r7,r11,14
        let quad = rotate_in_byte(mid | high) | (b3 & 0xFFFF_FFFE); // rlwinm r9,r5,0,0,30
        (u64::from(((quad as i32) >> 1) as u32) + u64::from(SIGNED_BIASES[2]), b3 & 1, 4)
    } else {
        // loc_82B46F7C: the raw 32-bit two's-complement word, never reaching the sign step.
        let b1 = code_byte(g, stream, 1)?;
        let b2 = code_byte(g, stream, 2)?;
        let b3 = code_byte(g, stream, 3)?;
        let b4 = code_byte(g, stream, 4)?;
        let wide = rotate_in_byte(rotate_in_byte(b1.rotate_left(8) | b2) | b3) | b4;
        return Ok((wide, 5));
    };
    // loc_82B46F20: an odd encoding is a negative. `subfic r11,r11,-1` is a 64-bit subtract; only
    // its low word is stored.
    let value = if low_bit != 0 { u64::MAX.wrapping_sub(magnitude) } else { magnitude };
    Ok((value as u32, length))
}

/// Decode one signed code at `stream`, store it at `out`, return its length (`sub_82B46EA0`).
pub fn decode_signed(g: &mut Guest, stream: u32, out: u32) -> Result<u64> {
    let (value, length) = signed_code(g, stream)?;
    g.set_u32(out, value)?; // stw r11,0(r4)
    Ok(u64::from(length))
}

// ---------------------------------------------------------------------- the run-length layer

/// A run's sub-object: `+0` its cursor object, `+4` the running sum, `+8` what is left of the run
/// (signed), `+12` the flag byte — set for a repeat run, clear for a literal run.
pub const RUN_CURSOR: u32 = 0;
/// `+4` — the running sum, returned by every [`run_length_step`].
pub const RUN_ACCUMULATED: u32 = 4;
/// `+8` — values left in the current run. Compared signed: zero or below means read a new count.
pub const RUN_REMAINING: u32 = 8;
/// `+12` — 1 for a repeat run, 0 for a literal run. Re-read from memory at every decision.
pub const RUN_FLAG: u32 = 12;
/// The cursor object's `+0`: the live byte pointer into the stream.
pub const CURSOR_STREAM: u32 = 0;

/// Decode one signed code into the running sum and advance the stream (`sub_82B46FB0`).
///
/// Returns the code's byte length, which is what the original leaves in `r3` — it is the callee's
/// return value and nothing overwrites it. The stream pointer is **re-read after the decode** even
/// though the decode never writes it; kept, since a cursor object that aliased the stream would see
/// the difference.
pub fn decode_signed_into(g: &mut Guest, object: u32) -> Result<u64> {
    let cursor = g.u32(object + RUN_CURSOR)?; // lwz r30,0(r3)
    let stream = g.u32(cursor + CURSOR_STREAM)?; // lwz r3,0(r30)
    let (decoded, length) = signed_code(g, stream)?; // bl 0x82b46ea0
    let reloaded = g.u32(cursor + CURSOR_STREAM)?; // lwz r10,0(r30)
    g.set_u32(cursor + CURSOR_STREAM, reloaded.wrapping_add(length))?; // stw r9,0(r30)
    let accumulated = g.u32(object + RUN_ACCUMULATED)?; // lwz r10,4(r31)
    g.set_u32(object + RUN_ACCUMULATED, accumulated.wrapping_add(decoded))?; // stw r8,4(r31)
    Ok(u64::from(length))
}

/// One step of the run-length decoder: return the current running sum (`sub_82B47010`).
///
/// When the run is exhausted a count code is read. The two stores that follow it — flag 1, remaining
/// `count + 1` — are overwritten straight away for a negative count, and those dead stores stay,
/// because a concurrent reader could see them. The flag is then **re-read from memory** twice, and
/// exactly one of the two [`decode_signed_into`] call sites runs after a fresh count.
pub fn run_length_step(g: &mut Guest, object: u32) -> Result<u64> {
    // lwz r11,8(r3) ; cmpwi cr6,r11,0 ; bgt -- signed
    if g.u32(object + RUN_REMAINING)? as i32 <= 0 {
        let cursor = g.u32(object + RUN_CURSOR)?;
        let stream = g.u32(cursor + CURSOR_STREAM)?;
        let (count, length) = signed_code(g, stream)?; // bl 0x82b46ea0
        let reloaded = g.u32(cursor + CURSOR_STREAM)?; // lwz r10,0(r30) -- reloaded
        g.set_u32(cursor + CURSOR_STREAM, reloaded.wrapping_add(length))?;
        g.set_u32(object + RUN_REMAINING, count.wrapping_add(1))?; // addi r7,r11,1 ; stw
        g.set_u8(object + RUN_FLAG, 1)?; // stb r9,12(r31)
        if (count as i32) < 0 {
            // A literal run: the two stores above are overwritten, and they stay as dead stores.
            g.set_u8(object + RUN_FLAG, 0)?; // stb r10,12(r31)
            g.set_u32(object + RUN_REMAINING, 1u32.wrapping_sub(count))?; // subfic r9,r11,1
        }
        if g.u8(object + RUN_FLAG)? != 0 {
            decode_signed_into(g, object)?; // the repeat run's one delta
        }
    }
    // loc_82B47090 -- the flag again, from memory.
    if g.u8(object + RUN_FLAG)? == 0 {
        decode_signed_into(g, object)?; // a literal run decodes on every call
    }
    let remaining = g.u32(object + RUN_REMAINING)?; // lwz r11,8(r31)
    let result = g.u32(object + RUN_ACCUMULATED)?; // lwz r3,4(r31)
    g.set_u32(object + RUN_REMAINING, remaining.wrapping_sub(1))?; // addi -1 ; stw
    Ok(u64::from(result))
}

/// The four sub-objects of [`decode_four`], sixteen bytes apart from `+4`.
pub const FIELD_OFFSETS: [u32; 4] = [4, 20, 36, 52];

/// Step four run-length decoders and store their four values at `out` (`sub_82B47658`).
///
/// `out` is `r3` and `input` is `r4`. The four may share one cursor object, in which case they
/// consume one stream in turn — the order is the original's, sub-object 0 first.
pub fn decode_four(g: &mut Guest, out: u32, input: u32) -> Result<()> {
    for (i, offset) in FIELD_OFFSETS.iter().enumerate() {
        let value = run_length_step(g, input.wrapping_add(*offset))?; // bl 0x82b47010
        g.set_u32(out + 4 * i as u32, value as u32)?; // stw r3,4i(r30)
    }
    Ok(())
}

// ------------------------------------------------------------------------ the paged bit cursor

/// Past this cursor a 15-bit field straddles the page end.
pub const SPLIT_POINT: i32 = 16_337;
/// Payload bits per page: 2,048 bytes less a 32-bit header. The cursor wraps here.
pub const PAGE_BITS: u32 = 16_352;
/// A page, in bytes. The buffer base steps by this on a wrap.
pub const PAGE_BYTES: u32 = 2_048;

const _: () = assert!(PAGE_BITS == PAGE_BYTES * 8 - 32, "a 32-bit header per page");
const _: () = assert!(SPLIT_POINT == PAGE_BITS as i32 - 15, "a 15-bit field fits below it");

/// `srawi r,x,3 ; addze`: divide by eight, truncating toward zero.
fn divide_by_eight_toward_zero(value: i32) -> i32 {
    let carry = value < 0 && (value as u32 & 7) != 0;
    (value >> 3) + i32::from(carry)
}

/// Three bytes at a byte offset, left-aligned in a word: `b0<<24 | b1<<16 | b2<<8`.
fn three_bytes_aligned(g: &Guest, buffer: u32, byte_offset: i32) -> Result<u32> {
    let at = buffer.wrapping_add(byte_offset as u32);
    let b0 = u32::from(g.u8(at)?);
    let b1 = u32::from(g.u8(at.wrapping_add(1))?);
    let b2 = u32::from(g.u8(at.wrapping_add(2))?);
    Ok(((((b0 << 8) | b1) << 8 | b2) << 8) & 0xFFFF_FF00)
}

/// Read the 15-bit field at the cursor and advance the cursor by it (`sub_82B50270`).
///
/// `base_field` is `r3`, holding the page buffer's address, and `cursor_field` is `r4`, holding the
/// bit cursor. The field is read at physical bit `cursor + 32`, past the page header; any part past
/// the split point comes from physical bit `cursor + width + 64`, past the *next* page's header. A
/// result at or past [`PAGE_BITS`] steps the base on one page and keeps the remainder — **once**, so a
/// field long enough to cross two pages leaves the cursor still past the end, as the original does.
pub fn advance_bit_cursor(g: &mut Guest, base_field: u32, cursor_field: u32) -> Result<()> {
    let cursor = g.u32(cursor_field)? as i32; // lwz r5,0(r4)
    let spill = if cursor > SPLIT_POINT { cursor.wrapping_sub(SPLIT_POINT) } else { 0 };
    let first_width = 15i32.wrapping_sub(spill); // subfic r11,r7,15
    let buffer = g.u32(base_field)?; // lwz r10,0(r3)

    let mut value = 0u32;
    if first_width > 0 {
        let position = cursor.wrapping_add(32); // addi r9,r5,32
        let byte_offset = divide_by_eight_toward_zero(position);
        let bit_in_group = position.wrapping_sub(byte_offset.wrapping_mul(8));
        let word = three_bytes_aligned(g, buffer, byte_offset)?;
        let shifted = word << (bit_in_group as u32 & 0x1F); // slw
        value = shifted >> ((32 - first_width) as u32 & 0x1F); // srw
    }

    let second_position = cursor.wrapping_add(first_width).wrapping_add(64);
    if spill > 0 {
        let already = value << (spill as u32 & 0x1F); // slw r6,r8,r7
        let byte_offset = divide_by_eight_toward_zero(second_position);
        let bit_in_group = second_position.wrapping_sub(byte_offset.wrapping_mul(8));
        let word = three_bytes_aligned(g, buffer, byte_offset)?;
        let shifted = word << (bit_in_group as u32 & 0x1F);
        value = (shifted >> (32i32.wrapping_sub(spill) as u32 & 0x1F)) | already;
    }

    let advanced = (cursor as u32).wrapping_add(value); // add r11,r5,r8
    if advanced < PAGE_BITS {
        return g.set_u32(cursor_field, advanced); // stw r11,0(r4)
    }
    g.set_u32(base_field, buffer.wrapping_add(PAGE_BYTES))?; // stw r10,0(r3)
    g.set_u32(cursor_field, advanced - PAGE_BITS) // stw r9,0(r4)
}

// ------------------------------------------------------------------- the packed stream header

/// `cmplwi cr6,r11,72` — an `'H'` prefix, which makes the header start four bytes later.
pub const HEADER_TAG: u8 = 72;
/// `stwu r1,-112(r1)` — the frame the bit reader lives in.
pub const HEADER_FRAME_BYTES: u32 = 112;
/// `addi r3,r1,80` — the reader's two words, handed to [`read_bits`] six times.
pub const HEADER_READER: u32 = 80;

/// Unpack a 64-bit packed stream header into a four-field description (`sub_82B31D90`).
///
/// Six fields are read MSB-first with [`read_bits`] — 4, 4, 6, 18, 3 and 29 bits — and four are
/// kept: the second nibble at `out + 12`, the 6-bit field **plus one** as a byte at `out + 0`, the
/// 18-bit field at `out + 4`, and the 29-bit field at `out + 8`. The first nibble and the 3-bit field
/// are read and discarded. An `'H'` tag byte in front skips four bytes.
///
/// The frame is reproduced, because the reader is guest memory that [`read_bits`] reads and writes.
pub fn unpack_stream_header(g: &mut Guest, stream: u32, out: u32, sp: u32) -> Result<()> {
    // lbz r11,0(r3) ; cmplwi cr6,r11,72 ; bne ; addi r3,r3,4 -- before the frame moves
    let body = if g.u8(stream)? == HEADER_TAG { stream.wrapping_add(4) } else { stream };
    let frame = sp.wrapping_sub(HEADER_FRAME_BYTES); // stwu r1,-112(r1)
    g.set_u32(frame, sp)?;
    let reader = frame + HEADER_READER;
    g.set_u32(reader + READER_DATA, body)?; // stw r3,80(r1)
    g.set_u32(reader + READER_CURSOR, 0)?; // li r11,0 ; stw r11,84(r1)

    read_bits(g, reader, 4)?; // the first nibble, discarded
    let nibble = read_bits(g, reader, 4)?;
    g.set_u32(out + 12, nibble as u32)?; // stw r3,12(r31)
    let count = read_bits(g, reader, 6)?;
    g.set_u8(out, count.wrapping_add(1) as u8)?; // addi r10,r3,1 ; stb r10,0(r31)
    let field18 = read_bits(g, reader, 18)?;
    g.set_u32(out + 4, field18 as u32)?; // stw r3,4(r31)
    read_bits(g, reader, 3)?; // discarded
    let field29 = read_bits(g, reader, 29)?;
    g.set_u32(out + 8, field29 as u32) // stw r3,8(r31)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x4000_0000;

    // ---------------------------------------------------------------- independent encoders

    /// The unsigned code, written from the module description rather than from the decoder.
    fn encode_unsigned(v: u32) -> Vec<u8> {
        if v < 192 {
            vec![v as u8]
        } else if v < 12_480 {
            let x = v - 192;
            vec![0xC0 | (x >> 8) as u8, x as u8]
        } else if v < 798_912 {
            let x = v - 12_480;
            vec![0xF0 | (x >> 16) as u8, (x >> 8) as u8, x as u8]
        } else if v - 798_912 < 0x300_0000 {
            let x = v - 798_912;
            vec![0xFC | (x >> 24) as u8, (x >> 16) as u8, (x >> 8) as u8, x as u8]
        } else {
            let mut out = vec![0xFF];
            out.extend_from_slice(&v.to_be_bytes());
            out
        }
    }

    /// The signed zigzag code, likewise independent: sign in the last byte's low bit.
    fn encode_signed(n: i32) -> Vec<u8> {
        let (m, s) = if n >= 0 { (n as u32, 0u32) } else { (!n as u32, 1u32) };
        if m < 96 {
            vec![(2 * m + s) as u8]
        } else if m < 6_240 {
            let x = 2 * (m - 96) + s;
            vec![0xC0 | (x >> 8) as u8, x as u8]
        } else if m < 399_456 {
            let x = 2 * (m - 6_240) + s;
            vec![0xF0 | (x >> 16) as u8, (x >> 8) as u8, x as u8]
        } else if m - 399_456 < 0x180_0000 {
            let x = 2 * (m - 399_456) + s;
            vec![0xFC | (x >> 24) as u8, (x >> 16) as u8, (x >> 8) as u8, x as u8]
        } else {
            let mut out = vec![0xFF];
            out.extend_from_slice(&n.to_be_bytes());
            out
        }
    }

    fn with_bytes(bytes: &[u8]) -> Guest {
        let mut g = Guest::single(BASE, 0x4000);
        g.set_span(BASE + 0x100, bytes).unwrap();
        g
    }

    #[test]
    fn the_unsigned_code_round_trips_across_every_width_boundary() {
        let values = [
            0u32, 1, 191, 192, 193, 12_479, 12_480, 12_481, 798_911, 798_912, 798_913,
            798_912 + 0x2FF_FFFF, 798_912 + 0x300_0000, 0x7FFF_FFFF, 0xFFFF_FFFF, 1_000, 500_000,
        ];
        for v in values {
            let code = encode_unsigned(v);
            let g = with_bytes(&code);
            assert_eq!(unsigned_code(&g, BASE + 0x100).unwrap(), (v, code.len() as u32), "{v}");
        }
    }

    #[test]
    fn the_signed_code_round_trips_across_every_width_boundary() {
        let values = [
            0i32, 1, -1, 95, -96, 96, -97, 6_239, -6_240, 6_240, -6_241, 399_455, -399_456,
            399_456, -399_457, 399_456 + 0x17F_FFFF, -(399_456 + 0x17F_FFFF) - 1, i32::MAX,
            i32::MIN, 12_345, -54_321,
        ];
        for n in values {
            let code = encode_signed(n);
            let g = with_bytes(&code);
            let (value, length) = signed_code(&g, BASE + 0x100).unwrap();
            assert_eq!((value as i32, length), (n, code.len() as u32), "{n}");
        }
    }

    #[test]
    fn every_width_starts_where_the_previous_one_ends() {
        // The biases make the widths contiguous: the smallest two-byte code is 192, the largest
        // one-byte code 191, and so on. A bias off by one leaves a gap or an overlap here.
        let first = |bytes: &[u8]| unsigned_code(&with_bytes(bytes), BASE + 0x100).unwrap().0;
        assert_eq!(first(&[0xBF]), 191);
        assert_eq!(first(&[0xC0, 0x00]), 192);
        assert_eq!(first(&[0xEF, 0xFF]), 12_479);
        assert_eq!(first(&[0xF0, 0x00, 0x00]), 12_480);
        assert_eq!(first(&[0xFB, 0xFF, 0xFF]), 798_911);
        assert_eq!(first(&[0xFC, 0x00, 0x00, 0x00]), 798_912);
    }

    #[test]
    fn the_five_byte_forms_are_the_raw_word_with_no_zigzag() {
        let g = with_bytes(&[0xFF, 0x80, 0x00, 0x00, 0x01]);
        assert_eq!(signed_code(&g, BASE + 0x100).unwrap(), (0x8000_0001, 5), "not zigzagged");
        assert_eq!(unsigned_code(&g, BASE + 0x100).unwrap(), (0x8000_0001, 5), "not biased");
    }

    #[test]
    fn the_guest_facing_decoders_store_the_value_and_return_the_length() {
        let mut g = with_bytes(&encode_signed(-7_000));
        assert_eq!(decode_signed(&mut g, BASE + 0x100, BASE + 0x10).unwrap(), 3);
        assert_eq!(g.u32(BASE + 0x10).unwrap() as i32, -7_000);

        let mut g = with_bytes(&encode_unsigned(70_000));
        assert_eq!(decode_unsigned(&mut g, BASE + 0x100, BASE + 0x10).unwrap(), 3);
        assert_eq!(g.u32(BASE + 0x10).unwrap(), 70_000);
    }

    // ------------------------------------------------------------------------ the bit reader

    const READER: u32 = BASE;
    const DATA: u32 = BASE + 0x100;

    /// An MSB-first reader that works one bit at a time — nothing like the byte-chunked original.
    fn model_bits(data: &[u8], cursor: u32, bits: u32) -> u32 {
        let mut v = 0u64;
        for i in 0..bits {
            let p = (cursor + i) as usize;
            v = (v << 1) | u64::from((data[p / 8] >> (7 - p % 8)) & 1);
        }
        v as u32
    }

    fn pattern() -> Vec<u8> {
        let mut x = 0x9E37_79B9u32;
        (0..64)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn read_bits_matches_a_bit_at_a_time_model() {
        let data = pattern();
        for cursor in [0u32, 1, 3, 7, 8, 13, 100] {
            for bits in [1u32, 2, 5, 7, 8, 9, 15, 16, 17, 24, 31, 32] {
                let mut g = Guest::single(BASE, 0x400);
                g.set_span(DATA, &data).unwrap();
                g.set_u32(READER + READER_DATA, DATA).unwrap();
                g.set_u32(READER + READER_CURSOR, cursor).unwrap();

                let got = read_bits(&mut g, READER, u64::from(bits)).unwrap();

                assert_eq!(got as u32, model_bits(&data, cursor, bits), "cursor {cursor}, {bits} bits");
                assert_eq!(g.u32(READER + READER_CURSOR).unwrap(), cursor + bits, "cursor advanced");
            }
        }
    }

    #[test]
    fn a_zero_bit_read_still_loads_a_byte_and_leaves_the_cursor() {
        // The do-while runs once: take = 0, the byte is loaded, and the cursor is stored unchanged.
        // With the data unmapped the load is an error, which proves it happens.
        let mut g = Guest::single(BASE, 0x40);
        g.set_u32(READER + READER_DATA, BASE + 0x1000).unwrap(); // not mapped
        g.set_u32(READER + READER_CURSOR, 5).unwrap();
        assert!(read_bits(&mut g, READER, 0).is_err(), "the byte load happens");

        let mut g = Guest::single(BASE, 0x400);
        g.set_u32(READER + READER_DATA, DATA).unwrap();
        g.set_u32(READER + READER_CURSOR, 5).unwrap();
        assert_eq!(read_bits(&mut g, READER, 0).unwrap(), 0);
        assert_eq!(g.u32(READER + READER_CURSOR).unwrap(), 5);
    }

    // --------------------------------------------------------------------- the run-length layer

    const CURSOR_OBJ: u32 = BASE + 0x40;
    const RUN: u32 = BASE + 0x60;
    const STREAM: u32 = BASE + 0x200;

    fn run_guest(codes: &[i32]) -> Guest {
        let mut g = Guest::single(BASE, 0x1000);
        let bytes: Vec<u8> = codes.iter().flat_map(|c| encode_signed(*c)).collect();
        g.set_span(STREAM, &bytes).unwrap();
        g.set_u32(CURSOR_OBJ + CURSOR_STREAM, STREAM).unwrap();
        g.set_u32(RUN + RUN_CURSOR, CURSOR_OBJ).unwrap();
        g
    }

    /// Expand a stream of run codes the way the module note describes, independently of the port:
    /// a count `c >= 0` is one delta and `c + 1` outputs; a count `c < 0` is `1 - c` deltas, each
    /// with an output.
    fn model_runs(codes: &[i32], outputs: usize) -> Vec<i32> {
        let mut out = Vec::new();
        let mut sum = 0i32;
        let mut i = 0;
        while out.len() < outputs {
            let count = codes[i];
            i += 1;
            if count >= 0 {
                sum = sum.wrapping_add(codes[i]);
                i += 1;
                for _ in 0..=count {
                    out.push(sum);
                }
            } else {
                for _ in 0..(1 - count) {
                    sum = sum.wrapping_add(codes[i]);
                    i += 1;
                    out.push(sum);
                }
            }
        }
        out.truncate(outputs);
        out
    }

    #[test]
    fn a_repeat_run_then_a_literal_run_matches_the_model() {
        let codes = [2, 3, -1, 10, -4, 0, 100];
        let mut g = run_guest(&codes);
        let got: Vec<i32> = (0..6).map(|_| run_length_step(&mut g, RUN).unwrap() as u32 as i32).collect();
        assert_eq!(got, model_runs(&codes, 6));
        assert_eq!(got, vec![3, 3, 3, 13, 9, 109], "spelled out, so the model is checked too");
    }

    #[test]
    fn a_repeat_run_consumes_no_stream_after_its_first_value() {
        let codes = [3, 5];
        let mut g = run_guest(&codes);
        run_length_step(&mut g, RUN).unwrap();
        let after_first = g.u32(CURSOR_OBJ + CURSOR_STREAM).unwrap();
        assert_eq!(after_first, STREAM + 2, "the count and the delta, one byte each");
        for _ in 0..3 {
            assert_eq!(run_length_step(&mut g, RUN).unwrap(), 5);
        }
        assert_eq!(g.u32(CURSOR_OBJ + CURSOR_STREAM).unwrap(), after_first, "nothing read");
        assert_eq!(g.u8(RUN + RUN_FLAG).unwrap(), 1, "a repeat run keeps its flag set");
    }

    #[test]
    fn a_negative_count_leaves_the_flag_clear_and_the_run_long() {
        // count -3: remaining = 1 - (-3) = 4, and the first delta is already consumed on this call.
        let mut g = run_guest(&[-3, 1, 1, 1, 1]);
        run_length_step(&mut g, RUN).unwrap();
        assert_eq!(g.u8(RUN + RUN_FLAG).unwrap(), 0);
        assert_eq!(g.u32(RUN + RUN_REMAINING).unwrap(), 3, "four values, one already returned");
    }

    #[test]
    fn decode_signed_into_advances_the_stream_and_the_sum() {
        let mut g = run_guest(&[-5_000]);
        g.set_u32(RUN + RUN_ACCUMULATED, 7_000).unwrap();
        assert_eq!(decode_signed_into(&mut g, RUN).unwrap(), 2, "a two-byte code");
        assert_eq!(g.u32(RUN + RUN_ACCUMULATED).unwrap(), 2_000);
        assert_eq!(g.u32(CURSOR_OBJ + CURSOR_STREAM).unwrap(), STREAM + 2);
    }

    #[test]
    fn decode_four_steps_each_sub_object_in_turn() {
        // Four sub-objects, each with its own cursor and stream, and one output word each.
        let mut g = Guest::single(BASE, 0x2000);
        let input = BASE + 0x80;
        let out = BASE + 0x10;
        for (i, offset) in FIELD_OFFSETS.iter().enumerate() {
            let cursor = BASE + 0x400 + 0x10 * i as u32;
            let stream = BASE + 0x800 + 0x100 * i as u32;
            let codes = [0, (i as i32 + 1) * 11]; // a repeat run of one, delta 11, 22, 33, 44
            let bytes: Vec<u8> = codes.iter().flat_map(|c| encode_signed(*c)).collect();
            g.set_span(stream, &bytes).unwrap();
            g.set_u32(cursor + CURSOR_STREAM, stream).unwrap();
            g.set_u32(input + offset + RUN_CURSOR, cursor).unwrap();
        }
        decode_four(&mut g, out, input).unwrap();
        for i in 0..4u32 {
            assert_eq!(g.u32(out + 4 * i).unwrap(), (i + 1) * 11, "field {i}");
        }
    }

    // ------------------------------------------------------------------- the paged bit cursor

    const BASE_FIELD: u32 = BASE;
    const CURSOR_FIELD: u32 = BASE + 4;
    const PAGES: u32 = BASE + 0x1000;

    /// The same field read from the *logical* payload stream: logical bit L lives at physical bit
    /// `L + 32` on the first page and `L - 16352 + 16384 + 32` on the next. Independent of the
    /// port's two-read split.
    fn model_field(pages: &[u8], cursor: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..15u32 {
            let l = cursor + i;
            let p = if l < PAGE_BITS { l + 32 } else { l - PAGE_BITS + 8 * PAGE_BYTES + 32 } as usize;
            v = (v << 1) | u32::from((pages[p / 8] >> (7 - p % 8)) & 1);
        }
        v
    }

    fn page_guest(pages: &[u8], cursor: u32) -> Guest {
        let mut g = Guest::single(BASE, 0x4000);
        g.set_span(PAGES, pages).unwrap();
        g.set_u32(BASE_FIELD, PAGES).unwrap();
        g.set_u32(CURSOR_FIELD, cursor).unwrap();
        g
    }

    #[test]
    fn the_cursor_advances_by_the_field_the_logical_model_reads() {
        let mut x = 0x1234_5678u32;
        let pages: Vec<u8> = (0..2 * PAGE_BYTES)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        // Across the page, including every cursor where the field straddles the split point.
        let cursors = (0..40).map(|k| k * 409).chain(16_320..16_352);
        for cursor in cursors {
            let mut g = page_guest(&pages, cursor);
            advance_bit_cursor(&mut g, BASE_FIELD, CURSOR_FIELD).unwrap();

            let advanced = cursor + model_field(&pages, cursor);
            let (want_base, want_cursor) = if advanced < PAGE_BITS {
                (PAGES, advanced)
            } else {
                (PAGES + PAGE_BYTES, advanced - PAGE_BITS)
            };
            assert_eq!(g.u32(CURSOR_FIELD).unwrap(), want_cursor, "cursor {cursor}");
            assert_eq!(g.u32(BASE_FIELD).unwrap(), want_base, "base, cursor {cursor}");
        }
    }

    #[test]
    fn a_wrap_steps_the_base_once_and_keeps_the_remainder() {
        // A field of 0x7FFF at a cursor of 16,000 runs 32,767 bits on: one page step, and the
        // remainder still past the page end — the original subtracts only once.
        let mut pages = vec![0u8; 2 * PAGE_BYTES as usize];
        for i in 0..15u32 {
            let p = (16_000 + 32 + i) as usize;
            pages[p / 8] |= 0x80 >> (p % 8);
        }
        let mut g = page_guest(&pages, 16_000);
        advance_bit_cursor(&mut g, BASE_FIELD, CURSOR_FIELD).unwrap();
        assert_eq!(g.u32(BASE_FIELD).unwrap(), PAGES + PAGE_BYTES);
        assert_eq!(g.u32(CURSOR_FIELD).unwrap(), 16_000 + 0x7FFF - PAGE_BITS);
        assert!(g.u32(CURSOR_FIELD).unwrap() >= PAGE_BITS, "still past the end: subtracted once");
    }

    #[test]
    fn the_page_constants_are_what_the_header_implies() {
        assert_eq!(PAGE_BITS, 2048 * 8 - 32);
        assert_eq!(SPLIT_POINT, 16_352 - 15);
        assert_eq!(divide_by_eight_toward_zero(-1), 0, "toward zero, not floor");
        assert_eq!(divide_by_eight_toward_zero(-9), -1);
        assert_eq!(divide_by_eight_toward_zero(17), 2);
    }

    // ------------------------------------------------------------------ the packed stream header

    /// Pack the six fields the way the module note describes, MSB first — independent of the reader.
    fn pack_header(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> [u8; 8] {
        ((a << 60) | (b << 56) | (c << 50) | (d << 32) | (e << 29) | f).to_be_bytes()
    }

    #[test]
    fn the_header_unpacks_its_four_kept_fields_with_and_without_the_tag() {
        let fields = (0xA, 0x5, 0x2A, 0x2_ABCD, 5, 0x123_4567);
        let header = pack_header(fields.0, fields.1, fields.2, fields.3, fields.4, fields.5);
        for tagged in [false, true] {
            let mut g = Guest::single(BASE, 0x1000);
            let stream = BASE + 0x100;
            let mut bytes = if tagged { vec![b'H', 0xEE, 0xEE, 0xEE] } else { Vec::new() };
            bytes.extend_from_slice(&header);
            g.set_span(stream, &bytes).unwrap();
            let (out, sp) = (BASE + 0x40, BASE + 0x800);

            unpack_stream_header(&mut g, stream, out, sp).unwrap();

            assert_eq!(g.u8(out).unwrap(), 0x2B, "tagged {tagged}: the 6-bit field plus one");
            assert_eq!(g.u32(out + 4).unwrap(), 0x2_ABCD, "tagged {tagged}: 18 bits");
            assert_eq!(g.u32(out + 8).unwrap(), 0x123_4567, "tagged {tagged}: 29 bits");
            assert_eq!(g.u32(out + 12).unwrap(), 0x5, "tagged {tagged}: the SECOND nibble");
            let reader = sp - HEADER_FRAME_BYTES + HEADER_READER;
            assert_eq!(g.u32(reader + READER_CURSOR).unwrap(), 64, "all 64 bits consumed");
            assert_eq!(g.u32(sp - HEADER_FRAME_BYTES).unwrap(), sp, "the back chain");
        }
    }
}
