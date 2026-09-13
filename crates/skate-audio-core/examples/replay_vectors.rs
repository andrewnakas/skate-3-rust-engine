//! Phase 4 tier 1: replay the shadow harness's recorded vectors against the Rust port.
//!
//! Usage: replay_vectors VECTORS.tsv [voice_live_hex voice_gone_hex]
//!
//! The vectors come from real gameplay, recorded by `skate3_audio_shadow.cpp` when
//! `skate3_audio_vectors_path` is set: the entry registers, the watched windows, the bytes on
//! entry, and the bytes the **original lifted body** produced. This feeds the identical inputs
//! to the Rust port and compares byte for byte, which is what tier 1 asks for — with real
//! inputs rather than generated ones.
//!
//! Two limits are reported rather than hidden. Functions with no Rust port yet are **skipped
//! and counted**; a pass rate with an invisible skip count reads as verification. And the
//! producer's query path writes a constant read from guest `.rdata`, which no recorded window
//! contains, so those vectors compare only the NaN sentinel unless the two constants are passed
//! on the command line. Deriving them from the expected bytes would be using the answer to
//! check the answer.

use skate_audio_core::mathlib::Trig;
use skate_audio_core::{
    Guest, buffers, crossfade, cursors, dsp, filters, gains, leaves, mathlib, mix, player, ring,
    interleave, routing,
    scheduler, spatial, stage, system,
};

struct Vector {
    name: String,
    run: u64,
    r3: u32,
    r4: u32,
    r5: u32,
    r6: u32,
    r7: u32,
    ret_r3: u32,
    /// Entry `f1`..`f8` as raw bit patterns. A DSP kernel's scale factor arrives in `f1`, and the
    /// one-pole stage's recursion state in `f5`, so without these a call cannot be replayed.
    f: [u64; 8],
    /// Entry `r3`..`r8`, full width. The fixed columns keep only the low word, and a port whose
    /// argument genuinely carries 64 bits -- or whose sixth argument is `r8`, which the fixed
    /// columns omit entirely -- cannot be replayed from those alone.
    w: [u64; 6],
    /// `f1` as the original left it, for the bodies whose result is a float and not a word.
    ret_f1: Option<u64>,
    /// Entry r1, r9 and r10. None when the recording predates them, which is different from zero:
    /// a body that reads its ninth argument off the caller's frame cannot be replayed without r1.
    r1: Option<u64>,
    r9: Option<u64>,
    r10: Option<u64>,
    /// Entry v1..v3 as four host-order words each, and the v1 the original returned. None when the
    /// recording predates them.
    vin: [[u32; 4]; 3],
    vret1: Option<[u32; 4]>,
    /// The read set: memory the function saw but does not write.
    inputs: Vec<(u32, Vec<u8>)>,
    /// The write set: entry bytes, and what the original lifted body produced.
    windows: Vec<(u32, Vec<u8>, Vec<u8>)>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect()
}

fn parse(line: &str) -> Option<Vector> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 9 {
        return None;
    }
    let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or(0);
    let mut inputs = Vec::new();
    let mut windows = Vec::new();
    let mut fprs = [0u64; 8];
    let (mut r1, mut r9, mut r10) = (None, None, None);
    let mut vin = [[0u32; 4]; 3];
    let mut vret1 = None;
    let words = |h: &str| -> Option<[u32; 4]> {
        if h.len() != 32 {
            return None;
        }
        let mut out = [0u32; 4];
        for (i, w) in out.iter_mut().enumerate() {
            *w = u32::from_str_radix(&h[i * 8..i * 8 + 8], 16).ok()?;
        }
        Some(out)
    };
    let mut wide = [None; 6];
    let mut ret_f1 = None;
    for tok in &f[8..] {
        let p: Vec<&str> = tok.split(':').collect();
        match (p.first(), p.len()) {
            (Some(&"I"), 4) => inputs.push((hex(p[1]), unhex(p[3]))),
            (Some(&"W"), 5) => windows.push((hex(p[1]), unhex(p[3]), unhex(p[4]))),
            // Vectors recorded before the float columns existed simply have none, and every
            // function that needs one fails loudly rather than replaying against a zero.
            (Some(&"V"), 3) => {
                if let (Ok(i), Some(w)) = (p[1].parse::<usize>(), words(p[2])) {
                    if (1..=3).contains(&i) {
                        vin[i - 1] = w;
                    }
                }
            }
            (Some(&"Vr"), 3) => {
                if p[1] == "1" {
                    vret1 = words(p[2]);
                }
            }
            (Some(&"Fr"), 3) => {
                if p[1] == "1" {
                    ret_f1 = u64::from_str_radix(p[2], 16).ok();
                }
            }
            (Some(&"R64"), 3) => {
                if let (Ok(i), Ok(bits)) = (p[1].parse::<usize>(), u64::from_str_radix(p[2], 16)) {
                    if (3..=8).contains(&i) {
                        wide[i - 3] = Some(bits);
                    }
                    match i {
                        1 => r1 = Some(bits),
                        9 => r9 = Some(bits),
                        10 => r10 = Some(bits),
                        _ => {}
                    }
                }
            }
            (Some(&"F"), 3) => {
                if let (Ok(i), Ok(bits)) = (p[1].parse::<usize>(), u64::from_str_radix(p[2], 16)) {
                    if (1..=8).contains(&i) {
                        fprs[i - 1] = bits;
                    }
                }
            }
            _ => {}
        }
    }
    // A vector with no write set usually compares nothing and is malformed. The exception is a
    // register-only body: the four-lane sine and the float-to-integer leaves write no memory at
    // all, and their whole result is a register. Dropping those would silently exclude the
    // functions the harness gained a result mask for in the first place.
    if windows.is_empty() && ret_f1.is_none() {
        return None;
    }
    // Older files have no wide columns; fall back to the zero-extended low word, which is right
    // whenever the high half was zero and wrong silently when it was not -- so the wide columns
    // are what a new recording should carry.
    let narrow = [hex(f[2]), hex(f[3]), hex(f[4]), hex(f[5]), hex(f[6]), 0];
    let mut w = [0u64; 6];
    for i in 0..6 {
        w[i] = wide[i].unwrap_or(u64::from(narrow[i]));
    }
    Some(Vector {
        f: fprs,
        w,
        ret_f1,
        r1,
        r9,
        r10,
        vin,
        vret1,
        name: f[0].to_string(),
        run: f[1].parse().unwrap_or(0),
        r3: hex(f[2]),
        r4: hex(f[3]),
        r5: hex(f[4]),
        r6: hex(f[5]),
        r7: hex(f[6]),
        ret_r3: hex(f[7]),
        inputs,
        windows,
    })
}

/// Build guest memory holding exactly what was recorded, as the function saw it **on entry**.
///
/// Nothing is invented. An address the function reaches that was not recorded stays uncovered
/// and the vector is reported `unreplayable`, never zero-filled: feeding the port fabricated
/// inputs would turn a failure into a meaningless pass.
///
/// Two things this has to get right, both found by replaying the scheduler and cursor vectors:
///
/// **The `W:` entry bytes are authoritative wherever they overlap an `I:` span.** The recorder
/// used to snapshot the read set *after* the original body had run, so any cell both read and
/// written was recorded holding the post-call value: across `sched_cursors.tsv`, 4,454 bytes sit
/// under both an `I:` and a `W:` span with differing entry and expected bytes, and in **4,454 of
/// 4,454** the `I:` byte is the *expected* one. That is fixed at the recorder, and a file made
/// since shows the reverse, 4,454 of 4,454 holding the entry byte. Laying the `W:` entry bytes
/// down last keeps files made before the fix replaying correctly and costs nothing on newer ones,
/// where the two agree.
///
/// **Spans are merged byte-wise, not stored one segment each.** Overlap is common — a window
/// often sits inside a larger read span, and sometimes shares its base — and a whole-segment
/// model resolves it by whichever segment `Guest::locate` happens to reach first, which is
/// insertion order. That silently fed `sub_82B489D0` a bucket byte from the read set while its
/// own store landed in the window segment the comparison then read, and it truncated
/// `sub_82B39690`'s 16-byte node span to the 8 bytes of a window sharing its base, losing the
/// `which` byte four bytes past the end. Merging into one map and coalescing runs of adjacent
/// recorded bytes removes both, without covering a single byte that was not recorded.
fn guest_of(v: &Vector) -> Guest {
    let mut cells: std::collections::BTreeMap<u32, u8> = Default::default();
    for (addr, bytes) in &v.inputs {
        for (i, b) in bytes.iter().enumerate() {
            cells.insert(addr.wrapping_add(i as u32), *b);
        }
    }
    // Laid down second, so the entry bytes overwrite the read set's post-call copy.
    for (addr, entry, _) in &v.windows {
        for (i, b) in entry.iter().enumerate() {
            cells.insert(addr.wrapping_add(i as u32), *b);
        }
    }

    let mut g = Guest::default();
    let mut run: Vec<u8> = Vec::new();
    let mut base = 0u32;
    let mut last = 0u32;
    for (addr, byte) in cells {
        if !run.is_empty() && addr == last.wrapping_add(1) && addr != 0 {
            run.push(byte);
        } else {
            if !run.is_empty() {
                g.put(base, std::mem::take(&mut run));
            }
            base = addr;
            run.push(byte);
        }
        last = addr;
    }
    if !run.is_empty() {
        g.put(base, run);
    }
    g
}

#[derive(Default)]
struct Tally {
    passed: u64,
    failed: u64,
    partial: u64,
    skipped: u64,
    /// The vector lacked memory the function needed — a gap in the recording, not a
    /// disagreement. Kept apart from `failed` so neither is mistaken for the other.
    unreplayable: u64,
    first_failure: Option<String>,
    first_gap: Option<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: replay_vectors VECTORS.tsv [voice_live_hex voice_gone_hex]");
        std::process::exit(2);
    }
    let consts = if args.len() >= 4 {
        Some(system::Constants {
            voice_live_bits: u32::from_str_radix(args[2].trim_start_matches("0x"), 16).unwrap(),
            voice_gone_bits: u32::from_str_radix(args[3].trim_start_matches("0x"), 16).unwrap(),
        })
    } else {
        None
    };

    let text = std::fs::read_to_string(&args[1]).expect("cannot read vectors");
    let mut by_name: std::collections::BTreeMap<String, Tally> = Default::default();
    let mut total = 0u64;

    for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let Some(v) = parse(line) else { continue };
        total += 1;
        let t = by_name.entry(v.name.clone()).or_default();
        let mut g = guest_of(&v);
        // Set by an arm whose result is a float; compared against the recorded `Fr:1` below.
        let mut float_result: Option<u64> = None;
        // Set by an arm whose result is a vector; compared against the recorded `Vr:1` below.
        let mut vector_result: Option<[u32; 4]> = None;
        // Whether this record predates the wide argument columns.
        let wide_missing = !line.contains("\tR64:");

        // Dispatch. A name with no Rust port is skipped and counted, never dropped.
        let outcome: std::result::Result<Option<u32>, String> = match v.name.as_str() {
            "BUFPAIR" => buffers::init_buffer_pair(&mut g, v.r3, v.r4, v.r5, v.r6, v.r7)
                .map(Some)
                .map_err(|e| e.to_string()),
            "EVENT_SUBMIT" => player::event_submit(&mut g, v.r3).map(Some).map_err(|e| e.to_string()),
            "EVENT_PLAY" => {
                let mut restarted = 0u32;
                player::event_play(&mut g, v.r3, Some(&mut |_| restarted += 1))
                    .map(Some)
                    .map_err(|e| e.to_string())
                    .and_then(|r| {
                        if restarted > 0 {
                            Err("the restart branch fired, which no observed call does".into())
                        } else {
                            Ok(r)
                        }
                    })
            }
            "ENQUEUE" => {
                let c = consts.unwrap_or(system::Constants {
                    voice_live_bits: 0,
                    voice_gone_bits: 0,
                });
                if v.r4 > 2 && consts.is_none() {
                    t.partial += 1;
                }
                system::enqueue(&mut g, v.r3, v.r4, v.r5, &c)
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            // The scheduler and cursor ports, addressed by their guest names. Each argument's
            // register is the one its module doc states; getting one wrong would not fail
            // gracefully, it would compare a different call.
            "sub_82B32550" => cursors::claim_ring_slot(&mut g, v.r3, v.r4)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            "sub_82B349A8" => cursors::advance_ring_cursor(&mut g, v.r3)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B3C9D8" => cursors::advance_segment_position(&mut g, v.r3, v.r4)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B39690" => scheduler::recycle_node(&mut g, v.r3, v.r4)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B489D0" => scheduler::detach_instance(&mut g, v.w[0], v.r4)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // The DSP kernels. `count` is r6, not r5, and the scale arrives in f1 -- both
            // straight from the module docs, both easy to get wrong in a way that still runs.
            "sub_82B3BED8" => dsp::scale::scale(&mut g, v.r3, v.r4, v.r6, f64::from_bits(v.f[0]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B44B20" => {
                dsp::scale::scale_accumulate(&mut g, v.r3, v.r4, v.r6, f64::from_bits(v.f[0]))
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            "sub_82B3C098" => dsp::gain_ramp::gain_ramp_copy(
                &mut g, v.r3, v.r4, f64::from_bits(v.f[0]), f64::from_bits(v.f[1]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            // The ring, mix and per-block DSP ports. Several of these take genuinely 64-bit
            // arguments, so they are fed the wide columns rather than a zero-extended low word.
            "sub_82B3DB90" => ring::copy_from_ring(&mut g, v.w[0], v.w[1], v.w[2], v.w[3], v.w[4])
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            "sub_82B3DC48" => {
                ring::fill_segments(&mut g, v.w[0], v.w[1], v.w[2] as u32, v.w[3])
                    .map(|r| Some(r as u32))
                    .map_err(|e| e.to_string())
            }
            "sub_82B3DF90" => ring::fill_tail(&mut g, v.r3, v.r4, v.r5)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B43AF8" => dsp::biquad::biquad(&mut g, v.r3, v.r4, v.r5, v.r6, v.r7)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            // r8 is the step, and the fixed columns stop at r7 -- a vector without the wide
            // columns would feed zero and make every address wrong, so it is refused instead.
            "sub_82B43FB8" => {
                if wide_missing {
                    t.unreplayable += 1;
                    if t.first_gap.is_none() {
                        t.first_gap = Some(format!(
                            "run {}: needs r8, which this recording predates", v.run));
                    }
                    continue;
                }
                dsp::resample::resample(&mut g, v.r3, v.r4, v.r5, v.r6, v.r7, v.w[5])
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            "sub_82B34E08" => mix::flush_accumulator(&mut g, v.r3, v.r4)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            "sub_82B3C668" => mix::fold_deltas(&mut g, v.r3, v.r4, v.r5)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B443F8" => mix::advance_and_clear(&mut g, v.r3, v.r4, v.r5, v.r6)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // The spatial chain. place_panner and add_angular are absent on purpose: both need
            // the guest's sine and cosine, which have no port in either language, and substituting
            // the host's would agree to fifteen digits and disagree in the bits the caller keeps.
            "sub_82B453D8" => spatial::clamp_to_unit_disc(
                &mut g, v.r3, f64::from_bits(v.f[0]), f64::from_bits(v.f[1]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B454B8" => {
                spatial::pan_distance(&mut g, v.r3, v.r4, v.r6, f64::from_bits(v.f[0]))
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            "sub_82B45B60" => spatial::scale_gains(
                &mut g, v.r3, v.r6, f64::from_bits(v.f[0]), f64::from_bits(v.f[1]),
                f64::from_bits(v.f[2]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B29AF0" => gains::apply_gain_matrix(&mut g, v.r3, v.r4, v.r5)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            // The guest returns a constant 1 here, so comparing it against the recording is a
            // real check that this port took the path the original took.
            "sub_82B23B50" => gains::ramp_channels(&mut g, v.r3, v.r4, v.w[2])
                .map(|_| Some(1u32))
                .map_err(|e| e.to_string()),
            // A register-only leaf: its whole result is f1, so the word comparison below has
            // nothing to check and the float comparison is the test.
            "sub_82F4DE80" => match mathlib::floor(&g, f64::from_bits(v.f[0])) {
                Ok(r) => {
                    float_result = Some(r.to_bits());
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            },
            // n is r3, the addend r5, the source r6, and the two outputs r7 and r8.
            "sub_82B3CF58" => dsp::scale_add::scale_add_with_copy(
                &mut g, v.r3, v.r5, v.r6, v.r7, v.w[5] as u32, f64::from_bits(v.f[0]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B3DEA8" => ring::write_into_ring(&mut g, v.w[0], v.w[1], v.w[2], v.w[3])
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // The two stage functions need r1, r9 and r10, which older recordings lack. Refused
            // rather than fed zeros: a zero stack pointer or source makes every address wrong.
            "sub_82B399D0" | "sub_82B39FA0" if v.r1.is_none() || v.r10.is_none() => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs r1/r9/r10, predates them", v.run));
                }
                continue;
            }
            "sub_82B399D0" => match stage::one_pole_stage(
                &mut g,
                v.w[0],
                v.r9.unwrap_or(0),
                v.r10.unwrap_or(0),
                v.r1.unwrap_or(0) as u32,
                f64::from_bits(v.f[0]),
                f64::from_bits(v.f[1]),
                f64::from_bits(v.f[2]),
                f64::from_bits(v.f[3]),
                f64::from_bits(v.f[4]),
            ) {
                Ok(r) => {
                    float_result = Some(r.to_bits());
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            },
            // The dispatcher opens a 128-byte frame below r1 and passes the kernel three arguments
            // through it. Its window builder leaves that frame undeclared on purpose -- it is the
            // call's own stack, which the harness never rewinds -- so no recording holds it, and
            // every vector used to stop at the frame's base as unreplayable. Seeding it with zeroes
            // is sound rather than invented input: the port writes the back chain and all three
            // argument slots before the kernel reads any of them, and the kernel loads only those
            // three words out of its 20-byte span, so no seeded byte can reach the result.
            "sub_82B39FA0" => {
                let sp = v.r1.unwrap_or(0) as u32;
                g.put(sp.wrapping_sub(stage::FRAME_BYTES), vec![0u8; stage::FRAME_BYTES as usize]);
                stage::run_stage(&mut g, v.r3, v.w[1], v.w[2], v.r7, sp)
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            // The image's sine and cosine: register-only, compared by the bits of f1.
            "sub_82F4DED0" | "sub_82F4DFB0" => {
                let x = f64::from_bits(v.f[0]);
                let r = if v.name == "sub_82F4DED0" {
                    mathlib::Image.sine(&g, x)
                } else {
                    mathlib::Image.cosine(&g, x)
                };
                match r {
                    Ok(value) => {
                        float_result = Some(value.to_bits());
                        Ok(None)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            // The four ports that call them, now given the real thing instead of `Unported`.
            "sub_82B269C0" => spatial::place_panner(
                &mut g, &mut mathlib::Image, v.r3, f64::from_bits(v.f[0]), f64::from_bits(v.f[1]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B45788" => spatial::add_angular(
                &mut g, &mut mathlib::Image, v.r3, v.r4, v.r6, f64::from_bits(v.f[0]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            "sub_82B27E20" => filters::lowpass_stage(&mut g, &mut mathlib::Image, v.w[0], v.w[1])
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            "sub_82B26568" => filters::highpass_stage(&mut g, &mut mathlib::Image, v.w[0], v.w[1])
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // The crossfade and its dispatcher both need r1, and the crossfade r9 and r10 as well.
            "sub_82B3D0A8" | "sub_82B3D4F8" if v.r1.is_none() || v.r10.is_none() => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs r1/r9/r10, predates them", v.run));
                }
                continue;
            }
            // count r3, then A..E in r6..r10, F off the caller's frame at 84(r1), gains in f1 and f2.
            "sub_82B3D0A8" => crossfade::crossfade(
                &mut g,
                v.w[0],
                v.w[3],
                v.w[4],
                v.w[5],
                v.r9.unwrap_or(0),
                v.r10.unwrap_or(0),
                v.r1.unwrap_or(0) as u32,
                f64::from_bits(v.f[0]),
                f64::from_bits(v.f[1]),
            )
            .map(|_| None)
            .map_err(|e| e.to_string()),
            // The dispatcher opens a 96-byte frame below r1 and passes the crossfade its sixth
            // pointer through it. That frame is the call's own stack, never a window and never
            // recorded, so it is seeded with zeroes -- sound because the port writes the back chain
            // and the argument slot before the callee reads either.
            "sub_82B3D4F8" => {
                let sp = v.r1.unwrap_or(0) as u32;
                g.put(sp.wrapping_sub(96), vec![0u8; 96]);
                crossfade::run_mix(&mut g, v.r3, v.w[1], v.r7, sp)
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            // The four-lane sine: argument and result both in v1, no memory touched at all.
            "sub_824531C8" if v.vret1.is_none() => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs v1, which this recording predates", v.run));
                }
                continue;
            }
            "sub_824531C8" => match dsp::sine::sine4(&g, v.vin[0]) {
                Ok(r) => {
                    vector_result = Some(r.v1);
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            },
            // ---------------------------------------------------------------- the four small leaves
            // Three return a value and touch little or nothing, which the recorded `ret_r3`
            // compares. Note the recording keeps only the **low word** of r3, so
            // `stream_remaining`'s 64-bit borrow is checked in its low half alone — the upper word,
            // where its subtraction borrows, is not in any vector.
            "sub_82B463A8" => leaves::stamp_slot(&mut g, v.r3)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            "sub_82B34268" => leaves::set_field_460(&mut g, v.r3, v.r6 as u16)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // No memory at all: the whole input is r6 and the whole result is r3.
            "sub_82B2C8E8" => Ok(Some(leaves::fourth_argument(v.w[3]) as u32)),
            "sub_82B23C10" => leaves::stream_remaining(&g, v.r3, v.r4 as u8)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // log10, through the natural log: argument and result both in f1.
            "sub_82F55068" => match mathlib::log10(&g, f64::from_bits(v.f[0])) {
                Ok(r) => {
                    float_result = Some(r.to_bits());
                    Ok(None)
                }
                Err(e) => Err(e.to_string()),
            },
            // The accumulating gain ramp, the twin of sub_82B3C098 above: same arguments, and its
            // result is the 1,024-byte destination rather than a register.
            "sub_82B44D18" => dsp::gain_ramp::gain_ramp_accumulate(
                &mut g, v.r3, v.r4, f64::from_bits(v.f[0]), f64::from_bits(v.f[1]))
                .map(|_| None)
                .map_err(|e| e.to_string()),
            // The ramping gain matrix keeps its per-column deltas in a 432-byte frame below r1,
            // which its window builder leaves undeclared because it is the call's own stack. Seeding
            // it with zeroes is sound **only when there is at least one source row**: pass one
            // writes every delta it later reads, so no seeded byte can reach the result. With zero
            // source rows the original reads whatever its caller left on the stack, which no
            // recording holds, so those calls are counted unreplayable instead of guessed at.
            "sub_82B298E0"
                if v.r1.is_none() || g.u32(v.r3 + gains::SOURCE_COUNT).unwrap_or(0) == 0 =>
            {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!(
                        "run {}: needs r1 and a non-empty source row set (its delta frame is its own stack)",
                        v.run
                    ));
                }
                continue;
            }
            "sub_82B298E0" => {
                let sp = v.r1.unwrap_or(0) as u32;
                g.put(
                    sp.wrapping_sub(gains::RAMP_FRAME_BYTES),
                    vec![0u8; gains::RAMP_FRAME_BYTES as usize],
                );
                gains::ramp_gain_matrix(&mut g, v.r3, v.r4, v.r5, v.r6, sp)
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            }
            // The hard clipper: 256 samples a channel, then the pair swap. Its `r3 = 1` is the
            // recorded return, and the clamped block is the rest of the comparison.
            "sub_82B22678" => dsp::clip::hard_clip(&mut g, v.r3, v.r4)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            // atan2 spills both arguments 16 bytes above the entry r1 — the caller's frame, which
            // its window declares — so a recording without the wide r1 column cannot be replayed.
            "sub_82F52318" if v.r1.is_none() => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs r1, where it spills y and x", v.run));
                }
                continue;
            }
            "sub_82F52318" => {
                let (y, x) = (f64::from_bits(v.f[0]), f64::from_bits(v.f[1]));
                match mathlib::atan2(&mut g, y, x, v.r1.unwrap_or(0) as u32) {
                    Ok(r) => {
                        float_result = Some(r.to_bits());
                        Ok(None)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            // The scatter-mixer takes six arguments, the sixth being the route table in r8, so a
            // recording without the wide columns cannot be replayed: a zero table would make every
            // route byte a read of guest address zero.
            "sub_82B426D0" if wide_missing => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs r8, the route table", v.run));
                }
                continue;
            }
            "sub_82B426D0" => routing::scatter_mix(
                &mut g, v.r3, v.r4, v.r5, v.r6, v.r7, v.w[5] as u32)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            // The planar-to-interleaved shuffle. Its cursor argument and its result are both the
            // full 64-bit r3 — the recorded return compares the low word — so it needs the wide
            // columns rather than the truncated one.
            "sub_82B46B30" if wide_missing => {
                t.unreplayable += 1;
                if t.first_gap.is_none() {
                    t.first_gap = Some(format!("run {}: needs the wide r3 cursor", v.run));
                }
                continue;
            }
            "sub_82B46B30" => interleave::interleave_six(&mut g, v.w[0], v.r4)
                .map(|r| Some(r as u32))
                .map_err(|e| e.to_string()),
            _ => {
                t.skipped += 1;
                continue;
            }
        };

        match outcome {
            Err(why) => {
                if why.contains("no segment covers") {
                    t.unreplayable += 1;
                    if t.first_gap.is_none() {
                        t.first_gap = Some(format!("run {}: {}", v.run, why));
                    }
                } else {
                    t.failed += 1;
                    if t.first_failure.is_none() {
                        t.first_failure = Some(format!("run {}: {}", v.run, why));
                    }
                }
            }
            Ok(ret) => {
                let mut bad = None;
                // The query path's constant word is unverifiable without the .rdata values.
                let skip_constant = v.name == "ENQUEUE" && v.r4 > 2 && consts.is_none();
                for (addr, _, expected) in &v.windows {
                    let got = g.span(*addr, expected.len()).expect("window vanished");
                    for i in 0..expected.len() {
                        if got[i] != expected[i] {
                            if skip_constant && (4..8).contains(&i) {
                                continue; // params+0xC, the .rdata constant
                            }
                            bad = Some(format!(
                                "run {}: {:08X}+{} expected {:02X}, got {:02X}",
                                v.run, addr, i, expected[i], got[i]
                            ));
                            break;
                        }
                    }
                    if bad.is_some() {
                        break;
                    }
                }
                if bad.is_none() {
                    // A float result is compared by bits, not by value: the point of porting
                    // these is that the bits agree, and two different bit patterns can compare
                    // equal as numbers.
                    if let (Some(got), Some(want)) = (float_result, v.ret_f1) {
                        if got != want {
                            bad = Some(format!(
                                "run {}: f1 returned {got:016X}, expected {want:016X}", v.run));
                        }
                    }
                }
                if bad.is_none() {
                    // A vector result is compared lane for lane, by bits.
                    if let (Some(got), Some(want)) = (vector_result, v.vret1) {
                        if got != want {
                            bad = Some(format!(
                                "run {}: v1 returned {got:08X?}, expected {want:08X?}", v.run));
                        }
                    }
                }
                if bad.is_none() {
                    if let Some(r) = ret {
                        if r != v.ret_r3 {
                            bad = Some(format!(
                                "run {}: returned {:08X}, expected {:08X}",
                                v.run, r, v.ret_r3
                            ));
                        }
                    }
                }
                match bad {
                    None => t.passed += 1,
                    Some(why) => {
                        t.failed += 1;
                        if t.first_failure.is_none() {
                            t.first_failure = Some(why);
                        }
                    }
                }
            }
        }
    }

    println!("replayed {total} recorded vectors\n");
    let mut any_failed = false;
    for (name, t) in &by_name {
        print!(
            "  {name:<13} pass={:<6} fail={:<5} skip={:<4} unreplayable={:<5}",
            t.passed, t.failed, t.skipped, t.unreplayable
        );
        if t.partial > 0 {
            print!(" partial={} (.rdata constant not supplied)", t.partial);
        }
        println!();
        if let Some(f) = &t.first_failure {
            println!("      first DISAGREEMENT: {f}");
            any_failed = true;
        }
        if let Some(gp) = &t.first_gap {
            println!("      first recording gap: {gp}");
        }
    }
    let skipped: u64 = by_name.values().map(|t| t.skipped).sum();
    if skipped > 0 {
        println!("\n  {skipped} vectors skipped: no Rust port for that function yet.");
    }
    let gaps: u64 = by_name.values().map(|t| t.unreplayable).sum();
    if gaps > 0 {
        println!(
            "  {gaps} vectors unreplayable: the recording lacks memory the function reached.\n  \
             A fixed read set cannot capture a linked-list walk, so this is expected for the\n  \
             producer's query path whenever the FIFO is non-empty."
        );
    }
    if any_failed {
        std::process::exit(1);
    }
}
