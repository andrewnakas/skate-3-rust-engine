//! What does each landing voice actually sound like, in level terms?
//!
//! A retail landing is five voices: the fixed impact (`0x447`), the 2×2 air-time ladder
//! (`0x35C`…`0x35F`), `sub_824B8D48`'s class voice, and two collision voices. Measured off a
//! playtest, retail's landings step **+4.8 dB (class 1) and +6.4 dB (class 2)** over class 0,
//! while this engine manages only +1.9 / +1.3. The MixMap send accounts for 3.0 dB of retail's
//! step, so the rest has to come from the sample content — which is what this measures.
//!
//! For each voice it resolves the Splice members the way the sink does, decodes each member's
//! stream, and reports the gain the sink would apply, the decoded peak, and the two combined.
//!
//!     cargo run --release -p skate-game --example landing_sample_levels -- [assets dir]

use skate_audio_core::authored::oneshot::Rand;
use skate_data::audio::splice::{LandingTuning, SpliceBanks, SpliceState};
use skate_data::collections::Collections;
use std::path::{Path, PathBuf};

const DEFAULT_ASSETS: &str = r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets";

fn db(v: f32) -> f32 {
    if v > 0.0 { 20.0 * v.log10() } else { -99.0 }
}

/// Decode one member's stream and return (peak, rms) in 0..1.
fn measure(bytes: &[u8], at: usize) -> Option<(f32, f32)> {
    let info = match skate_data::audio::describe(bytes, at) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("      describe @{at:#x}: {e}");
            return None;
        }
    };
    let mut decoder = skate_data::audio::ffmpeg::FfmpegDecoder::for_stream(&info);
    let pcm = match skate_data::audio::decode_stream(bytes, at, &mut decoder) {
        Ok(pcm) => pcm,
        Err(e) => {
            eprintln!("      decode @{at:#x}: {e}");
            return None;
        }
    };
    if pcm.is_empty() {
        eprintln!("      decode @{at:#x}: empty");
        return None;
    }
    let mut peak = 0.0f32;
    let mut energy = 0.0f64;
    for s in &pcm {
        let v = f32::from(*s) / 32768.0;
        peak = peak.max(v.abs());
        energy += f64::from(v) * f64::from(v);
    }
    Some((peak, (energy / pcm.len() as f64).sqrt() as f32))
}

/// Every stream start in the bank, sorted. A bank's streams are packed end to end, so the block
/// walker needs the *next* start as its end — handed the whole bank it reads past this stream and
/// into the next one's bytes, where the block sizes are garbage.
fn stream_starts(banks: &SpliceBanks, bank: &str) -> Vec<usize> {
    let Some(splc) = banks.bank(bank) else { return Vec::new() };
    let mut starts: Vec<usize> = (0..u16::MAX)
        .map_while(|index| splc.stream_offset(index))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Resolve a sample the way the sink does and report the loudest member.
fn voice(banks: &SpliceBanks, bank: &str, sample: u16, label: &str, starts: &[usize]) {
    let mut state = SpliceState::default();
    let mut rand = Rand::new(1);
    let members = match banks.resolve(bank, sample, &mut state, &mut rand) {
        Ok(m) => m,
        Err(e) => {
            println!("  {label:<22} sample {sample:#06x}  UNRESOLVED: {e}");
            return;
        }
    };
    let Some(raw) = banks.bank_bytes(bank) else {
        println!("  {label:<22} sample {sample:#06x}  bank {bank} not loaded");
        return;
    };
    // The audible level of the voice is the loudest member it opened.
    let mut best: Option<(f32, f32, f32)> = None;
    for m in &members {
        let at = m.stream_offset as usize;
        let end = starts.iter().copied().find(|&s| s > at).unwrap_or(raw.len());
        let Some((peak, _rms)) = measure(&raw[..end], at) else {
            continue;
        };
        let effective = peak * m.values.gain;
        if best.is_none_or(|(b, _, _)| effective > b) {
            best = Some((effective, peak, m.values.gain));
        }
    }
    // The authored gain is the part that does not need a decoder: it is member gain × the
    // container value × CONTACT_TRIM, i.e. exactly what the sink hands `play_oneshot`. If retail's
    // class step is authored as level, it is visible here.
    let sum: f32 = members.iter().map(|m| m.values.gain).sum();
    let loudest = members.iter().fold(0.0f32, |a, m| a.max(m.values.gain));
    let decoded = match best {
        Some((eff, peak, _)) => format!("  peak {:+6.1} dBFS -> {:+6.1} dBFS", db(peak), db(eff)),
        None => String::from("  (not decoded)"),
    };
    println!(
        "  {label:<22} sample {sample:#06x}  members {:<2}  loudest gain {loudest:.3} ({:+6.1} dB)  sum {sum:.3} ({:+6.1} dB){decoded}",
        members.len(),
        db(loudest),
        db(sum),
    );
}

fn main() {
    let assets = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_ASSETS));
    let vault = Collections::load(&assets).expect("vault");
    let landing = LandingTuning::load(&vault).expect("landing tuning");
    let banks = SpliceBanks::load(
        &Path::new(&assets).join("private/stock/data/audio/audiofiles.big"),
        &[skate_data::audio::splice::COLLISIONS_BANK],
        &[skate_data::audio::splice::COLLISIONS_BANK],
    )
    .expect("banks");
    let bank = landing.bank();
    let starts = stream_starts(&banks, bank);

    println!("\nThe impact voice (fixed for every landing):");
    voice(&banks, bank, landing.sample, "impact", &starts);

    println!("\nThe ladder voice (deck test x air >= {:.2} s):", landing.ladder_seconds);
    for test in 0..2 {
        for hard in 0..2 {
            let label = format!("test {test}, {}", if hard == 1 { "hard" } else { "soft" });
            voice(&banks, bank, landing.ladder[test][hard], &label, &starts);
        }
    }

    println!("\nThe class voice (`sub_824BA3F0`, kind 0) -- this is where retail's step should be:");
    for class in 0..3u32 {
        // Mode is the surface category only for class 2; 0 otherwise.
        for category in 0..if class >= 2 { 4u8 } else { 1 } {
            let Some(sample) = landing.class_sample(0, class, category) else {
                println!("  class {class} cat {category}: no sample");
                continue;
            };
            voice(&banks, bank, sample, &format!("class {class}, cat {category}"), &starts);
        }
    }
    println!();
}
