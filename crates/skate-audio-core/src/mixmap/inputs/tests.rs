//! The input writers against the image constants and the retail capture's extracts
//! (`.local/captures/extract/state.tsv` — frame, ms, 160 words from `state+192` — and
//! `mixmap/<ctrl>.tsv` — the controller's `MC` lines). A test whose data is absent says so.
//!
//! The bridge that writes the state runs in the frame's first half, the MixMap evaluation in the
//! second, and the capture numbers evaluations: the state dumped at `ST` frame *n* is the one the
//! controller inputs of evaluation *n + 1* were written from.

use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn extract() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/captures/extract");
    if dir.join("state.tsv").is_file() {
        Some(dir)
    } else {
        eprintln!("skipped: no capture extract at {}", dir.display());
        None
    }
}

fn states(dir: &PathBuf) -> HashMap<u32, Vec<u32>> {
    let mut out = HashMap::new();
    for line in std::fs::read_to_string(dir.join("state.tsv")).unwrap().lines() {
        let p: Vec<&str> = line.split('\t').collect();
        let words = p[2..162].iter().map(|w| u32::from_str_radix(w, 16).unwrap()).collect();
        out.insert(p[0].parse().unwrap(), words);
    }
    out
}

/// A controller's captured input words by evaluation.
fn inputs(dir: &PathBuf, ctrl: &str) -> HashMap<u32, [u32; 16]> {
    let mut out = HashMap::new();
    let text = std::fs::read_to_string(dir.join("mixmap").join(format!("{ctrl}.tsv"))).unwrap();
    for line in text.lines() {
        let p: Vec<&str> = line.split('\t').collect();
        let w: Vec<&str> = p[2].split_whitespace().collect();
        let at = w.iter().position(|&x| x == "|").unwrap();
        let mut words = [0u32; 16];
        for (i, v) in words.iter_mut().enumerate() {
            *v = u32::from_str_radix(w[at + 1 + i], 16).unwrap();
        }
        out.insert(p[0].parse().unwrap(), words);
    }
    out
}

fn word(s: &[u32], off: u32) -> u32 {
    s[((off - 192) / 4) as usize]
}
fn byte(s: &[u32], off: u32) -> bool {
    (word(s, off & !3) >> (8 * (3 - (off & 3)))) & 0xFF != 0
}

#[test]
fn the_state_controller_inputs_match_the_capture_word_for_word() {
    let Some(dir) = extract() else { return };
    let st = states(&dir);
    let mc = inputs(&dir, "4A26B1D0"); // 60010000
    let (mut compared, mut frames) = (0usize, 0usize);
    let mut slew = 0.0f32;
    for (&frame, s) in &st {
        let Some(want) = mc.get(&(frame + 1)) else { continue };
        let fields = StateFields {
            wheel_count_200: word(s, 200),
            ground_speed_208: f32::from_bits(word(s, 208)),
            com_speed_212: f32::from_bits(word(s, 212)),
            brake_336: byte(s, 336),
            manual_brake_339: byte(s, 339),
            trick_active_343: byte(s, 343),
            local_player_72: true,
            bail_camera_g16: want[11] != 0, // the G+16 byte is not in the dump
            vector_96: [0.0; 4],
            listener_32: Some([0.0; 4]),
        };
        for (id, value) in state_inputs(&fields, &mut slew, 1.0 / 60.0, DISTANCE_RATE, DISTANCE_CAP) {
            if id == 11 {
                continue;
            }
            assert_eq!(value, want[id as usize], "evaluation {} id {id}", frame + 1);
            compared += 1;
        }
        // Id 12 through sub_824B23C8 with the local player's flag set: `+684 == 1`.
        let id12 = if player_flag(true, word(s, 684), &[]) == 1 { 32767 } else { 0 };
        assert_eq!(id12, want[12], "evaluation {} id 12", frame + 1);
        frames += 1;
    }
    assert!(frames > 18_000, "{frames} frames");
    eprintln!("{compared} words over {frames} evaluations");
}

#[test]
fn rail_and_off_board_inputs_match_the_capture() {
    let Some(dir) = extract() else { return };
    let st = states(&dir);
    let rail = inputs(&dir, "4A26A8D0"); // 40010030
    let off = inputs(&dir, "4A26A930"); // 40010090
    let mut frames: Vec<_> = st.keys().copied().collect();
    frames.sort();
    let mut n = 0;
    for frame in frames {
        let s = &st[&frame];
        // `+342` is the bridge's copy of the previous frame's `+341`.
        let (g, g_prev) = (byte(s, 341), byte(s, 342));
        if let Some(w) = rail.get(&(frame + 1)) {
            let [(_, a), (_, b)] = rail_inputs(g, g_prev);
            assert_eq!((a, b), (w[0], w[1]), "rail, evaluation {}", frame + 1);
            n += 1;
        }
        if let Some(w) = off.get(&(frame + 1)) {
            assert_eq!(off_board_input(byte(s, 716)).1, w[0], "off-board, evaluation {}", frame + 1);
        }
    }
    assert!(n > 18_000);
}

#[test]
fn the_constants_are_the_images() {
    let dir = std::env::var("SKATE3_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets"))
        .join("private/stock/audio-runtime-image");
    if !dir.is_dir() {
        eprintln!("skipped: no image");
        return;
    }
    let read = |addr: u32| -> u32 {
        let page = std::fs::read(dir.join(format!("g_{:04X}.bin", addr >> 16))).unwrap();
        let o = (addr & 0xFFFF) as usize;
        u32::from_be_bytes(page[o..o + 4].try_into().unwrap())
    };
    for (i, &w) in SPEED_SCALES.iter().enumerate() {
        assert_eq!(read(0x822F_9520 + 4 * i as u32), w);
    }
    assert_eq!(read(0x8217_47FC), F32767);
    assert_eq!(read(0x8209_975C), HALF);
    assert_eq!(read(0x8211_61AC), SEVEN_TENTHS);
    assert_eq!((u64::from(read(0x822F_8D58)) << 32) | u64::from(read(0x822F_8D5C)), THREE_TENTHS);
    assert_eq!(read(0x8209_BE90), EPSILON);
    assert_eq!(read(0x822F_8960), ANGLE_SCALE);
    assert_eq!(read(0x822F_8904), INV_TWO_PI);
    assert_eq!(read(0x8216_DEE0), MINUS_ONE_BITS);
    assert_eq!(read(0x822F_B840), ACOS_ONE_PLUS);
    for (base, table) in [(0x822F_9820u32, ACOS_A), (0x822F_9830, ACOS_B), (0x822F_9840, ACOS_C), (0x822F_9850, ACOS_D)] {
        for (i, &w) in table.iter().enumerate() {
            assert_eq!(read(base + 4 * i as u32), w, "{base:#x}+{}", 4 * i);
        }
    }
}

#[test]
fn distance_slews_at_the_vault_rate_and_caps() {
    let mut slew = 0.0f32;
    let mut s = StateFields { vector_96: [3.0, 4.0, 0.0, 0.0], listener_32: Some([0.0; 4]), ..Default::default() };
    // 5 m away, 100 m/s × 0.01 s = 1 m per call.
    let id13 = |s: &StateFields, slew: &mut f32| state_inputs(s, slew, 0.01, DISTANCE_RATE, DISTANCE_CAP)[12];
    assert_eq!(id13(&s, &mut slew), (13, (1.0f32 / 35.0 * 32767.0) as u32));
    assert_eq!(slew, 1.0);
    for _ in 0..10 {
        id13(&s, &mut slew);
    }
    assert!((slew - 5.0).abs() < 1e-6, "{slew}");
    s.vector_96 = [100.0, 0.0, 0.0, 0.0];
    for _ in 0..40 {
        id13(&s, &mut slew);
    }
    assert_eq!(slew, 35.0, "capped");
    assert_eq!(id13(&s, &mut slew).1, 32767);
}

#[test]
fn listener_facing_is_zero_facing_the_listener_and_full_behind_it() {
    let l = [0.0, 0.0, 1.0, 0.0];
    assert_eq!(listener_facing(l, [0.0; 4], l).0, 0, "a·L = 1 → f1 = 0");
    let (w, v) = listener_facing([0.0, 0.0, -1.0, 0.0], [0.0; 4], l);
    assert_eq!((w, v), ((0.7f32 * 32767.0) as u32, 0.7f32), "a·L = −1, no up, b ⟂ L → 0.7");
    // |b·L| adds 0.3 × f1: fully behind with b along L saturates at 1.
    assert_eq!(listener_facing([0.0, 0.0, -1.0, 0.0], l, l).0, 32767);
}

#[test]
fn position_controller_on_the_followed_point_reads_zero_distance_and_speed() {
    // The capture's 60010010: emitter = listener pos1, same velocity → ids 0, 2 and 13 are 0.
    let listener = Listener {
        pos0: [0.0, 1.5, -3.0, 1.0],
        dir0: [0.0, 0.0, 1.0, 0.0],
        vel0: [0.0; 4],
        pos1: [0.0, 1.0, 0.0, 1.0],
        dir1: [0.0, 0.0, 1.0, 0.0],
        vel1: [1.0, 0.0, 2.0, 0.0],
        ..Default::default()
    };
    let emitter = Emitter { position_32: Some(listener.pos1), velocity_36: Some(listener.vel1), facing_28: Some([0.0, 0.0, 1.0, 0.0]) };
    let mut obj = ObjPos::default();
    let w: HashMap<u32, u32> = obj.update(&listener, &emitter, 0).into_iter().collect();
    assert_eq!(w[&0], 0, "|pos1 − emitter|");
    assert_eq!(w[&2], 0, "no horizontal offset from frame B's origin → angle 0");
    assert_eq!(w[&13], 0, "|v − vel1|");
    assert_eq!(f32::from_bits(w[&1]), obj.dist0_48);
    assert_eq!(w[&15] & 1, 1);
    // Straight ahead of frame A (pulled back 0.25 along its direction): angle 0.
    assert_eq!(w[&3], 0);
    // Inactive: the −1.0 distances and bit 0 cleared.
    let off: Vec<_> = obj.update(&listener, &Emitter::default(), 0xC000_0001);
    assert_eq!(off, vec![(3, 0), (1, 0xBF80_0000), (2, 0), (0, 0xBF80_0000), (15, 0xC000_0000)]);
}

#[test]
fn position_angles_turn_through_the_horizontal_plane() {
    let listener = Listener { dir0: [0.0, 0.0, 1.0, 0.0], dir1: [0.0, 0.0, 1.0, 0.0], ..Default::default() };
    let at = |x: f32, z: f32| {
        let mut obj = ObjPos { pullback_112: 0.0, ..Default::default() };
        let e = Emitter { position_32: Some([x, 5.0, z, 1.0]), ..Default::default() };
        let w: HashMap<u32, u32> = obj.update(&listener, &e, 0).into_iter().collect();
        (w[&3], w[&2])
    };
    // Ahead (+z): 0. To the side: a quarter turn, one side mirrored to 65535 − q.
    assert_eq!(at(0.0, 10.0).0, 0);
    let (right, _) = at(10.0, 0.0);
    let (left, _) = at(-10.0, 0.0);
    assert!((16380..=16390).contains(&right.min(left)), "{right} {left}");
    assert_eq!(right + left, 65535);
    let (behind, _) = at(0.0, -10.0);
    assert!((32760..=32775).contains(&behind), "{behind}");
}

#[test]
fn position_rates_flag_a_sign_change() {
    let listener = Listener { dir0: [0.0, 0.0, 1.0, 0.0], dir1: [0.0, 0.0, 1.0, 0.0], ..Default::default() };
    let mut obj = ObjPos::default();
    let v = Some([0.0, 0.0, 3.0, 0.0]);
    // Receding along +z: positive rates.
    for z in [5.0, 6.0] {
        obj.update(&listener, &Emitter { position_32: Some([0.0, 0.0, z, 1.0]), velocity_36: v, facing_28: None }, 1);
    }
    assert!(obj.rate1_96 > 0.0);
    // Approaching: the rates turn negative and both flags are raised.
    let w = obj.update(&listener, &Emitter { position_32: Some([0.0, 0.0, 4.0, 1.0]), velocity_36: v, facing_28: None }, 1);
    assert!(obj.rate1_96 < 0.0 && obj.rate0_104 < 0.0);
    assert!(w.contains(&(15, 0x8000_0001)) && w.contains(&(15, 0xC000_0001)), "{w:?}");
    assert_eq!(w.last(), Some(&(14, obj.rate0_104.to_bits())));
}

#[test]
fn contacts_landing_inputs_match_the_capture() {
    let Some(dir) = extract() else { return };
    let st = states(&dir);
    let mc = inputs(&dir, "4A26A8B0"); // 40010010
    let mut frames: Vec<_> = st.keys().copied().collect();
    frames.sort();
    let mut contacts = Contacts::default();
    let mut words = [0u32; 16];
    let (mut n, mut landings) = (0, 0);
    for frame in frames {
        let s = &st[&frame];
        let f = ContactsFields {
            airborne_332: byte(s, 332),
            grinding_341: byte(s, 341),
            wheel_contact_464: [byte(s, 464), byte(s, 465), byte(s, 466), byte(s, 467)],
            wheel_word_448: [0, 1, 2, 3].map(|i| word(s, 448 + 4 * i) as i32),
            wheel_material_620: [0, 1, 2, 3].map(|i| word(s, 620 + 4 * i)),
            local_player: true,
        };
        let writes = contacts.process(&f);
        if writes.len() > 3 {
            landings += 1;
        }
        for (id, v) in writes {
            words[id as usize] = v;
        }
        if let Some(w) = mc.get(&(frame + 1)) {
            for id in [1usize, 2, 6] {
                assert_eq!(words[id], w[id], "evaluation {} id {id}", frame + 1);
            }
            n += 1;
        }
    }
    assert!(n > 18_000 && landings >= 40, "{n} {landings}");
}

#[test]
fn jitter_walks_inside_its_bounds_with_the_shared_generator() {
    let mut g = crate::Guest::single(0x82FD_0000, 0x1_0000);
    for (i, w) in [0x1234_5678u32, 0x9ABC_DEF0, 0x0F1E_2D3C, 0x4B5A_6978, 0x8796_A5B4, 0xC3D2_E1F0].iter().enumerate() {
        g.set_u32(crate::grain::rng::STATE + 4 * i as u32, *w).unwrap();
    }
    let mut j = Jitter::retail();
    assert_eq!(j.channels.len(), 24);
    for _ in 0..2000 {
        let w = j.process(&mut g).unwrap();
        // Six enabled channels, ids 4, 3, 0, 1, 5, 2 in key order.
        assert_eq!(w.iter().map(|&(id, _)| id).collect::<Vec<_>>(), vec![4, 3, 0, 1, 5, 2]);
        for ch in &j.channels {
            let [c, r, max, _] = ch.params;
            assert!(ch.value >= c - r && ch.value <= c + r, "{ch:?}");
            assert!(ch.velocity.abs() <= max, "{ch:?}");
        }
        assert_eq!(w[4], (5, 0), "id 5 has all-zero params");
    }
}

#[test]
fn jitter_step_reduces_the_draw_mod_2001() {
    // The mulhwu sequence is r mod 2001 for every word.
    for r in [0u32, 1, 2000, 2001, 2002, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFE, 0xFFFF_FFFF, 123_456_789] {
        let hi = ((u64::from(r) * 0x0603_538B) >> 32) as u32;
        let q = ((r.wrapping_sub(hi) >> 1).wrapping_add(hi)) >> 10;
        assert_eq!(r - q * 2001, r % 2001, "{r}");
    }
    // A draw of 1000 (+0 after the offset) pushes by the minimum step only.
    let mut ch = Jitter::retail().channels[23]; // id 2: centre 16384, range 16383, 31000, 19000
    ch.step(1000); // velocity 19000, value 35384 > 32767: bounced
    assert_eq!(ch.value, 16384.0 + 19000.0 - 2.0 * (16384.0 + 19000.0 - 32767.0));
    assert_eq!(ch.velocity.to_bits(), (-19000.0f32).to_bits(), "bounced");
}

#[test]
fn the_listener_update_takes_camera_rows_and_the_first_record() {
    let mut l = Listener::default();
    let rec = PlayerRecord { position_0: [1.0, 2.0, 3.0, 1.0], facing_16: [0.0, 0.0, 1.0, 0.0], velocity_32: [4.0, 0.0, 0.0, 0.0], ..Default::default() };
    l.update([0.0, 0.0, 2.0, 0.0], [0.0, 1.0, 0.0, 1.0], true, 0.5, Some(&rec));
    assert_eq!(l.dir0[2], 1.0);
    assert_eq!(l.vel0, [0.0, 2.0, 0.0, 2.0], "(pos − prev) / dt over all four lanes");
    l.update([0.0, 0.0, 2.0, 0.0], [1.0, 1.0, 0.0, 1.0], false, 0.5, Some(&rec));
    assert_eq!(l.vel0, [0.0, 2.0, 0.0, 2.0], "kept when the camera counter did not change");
    assert_eq!((l.pos1, l.vel1, l.prev_pos1_80), (rec.position_0, rec.velocity_32, rec.position_0));
    let (a, b) = rec.emitters();
    assert_eq!(a.position_32, Some(rec.position_0));
    assert_eq!(b.velocity_36, Some(rec.velocity_80));
}
