//! Headless end-to-end check of the retail player-sound path: the exact worker setup
//! ([`super::prepare`]) and frame ([`super::sound::PlayerSound::frame`]) the game runs, fed
//! synthetic physics ticks, rendered block by block.
//!
//! `cargo test -p skate-game --bin skate3rust --locked -- --ignored headless --nocapture`
//! (assets from `SKATE_ASSETS`, defaulting to the owner's install). Optional `SKATE_HEADLESS_WAV`
//! writes the stereo output.

use crate::skate_audio::{PlayerAudioObservation, RetailAudioInputs};

const DEFAULT_ASSETS: &str = r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets";

/// A grounded roll at `speed` m/s straight along +X on four wheels.
fn rolling(tick: u64, speed: f32) -> PlayerAudioObservation {
    let mut retail = RetailAudioInputs::default();
    retail.dt = 1.0 / 60.0;
    retail.ground_speed = speed;
    retail.com_velocity = [speed, 0.0, 0.0];
    retail.linear_velocity = [speed, 0.0, 0.0];
    retail.wheel_count = 4;
    retail.wheel_contacts = [true; 4];
    retail.truck_contacts = [true; 2];
    retail.state_category = 100;
    retail.state = 100;
    retail.motion_200 = 1.0;
    retail.deck_rows = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    retail.effective_deck_up = [0.0, 1.0, 0.0];
    retail.ground_normal = [0.0, 1.0, 0.0];
    retail.wheel_contact_normals = [[0.0, 1.0, 0.0]; 4];
    retail.wheel_audio_surfaces = [3; 4];
    retail.scorable_id = -1;
    retail.deck_forward = [1.0, 0.0, 0.0];
    // A follow camera three metres behind and above, looking along the roll.
    retail.camera = Some(([1.0, 0.0, 0.0], [-3.0, 1.5, 0.0]));
    PlayerAudioObservation {
        tick,
        state: 100,
        board_speed: speed,
        rider_speed: speed,
        grounded: true,
        grinding: false,
        grind_family: 0,
        grind_substate: 0,
        grind_audio_surface: 0,
        grind_impact_speed: 0.0,
        wiping_out: false,
        landed: false,
        landing_impact_speed: 0.0,
        landing_clean: false,
        landing_sketchy: false,
        landing_type: 0,
        landing_spin: 0.0,
        landing_sideways_speed: 0.0,
        footstep_strength: 0.0,
        footstep_bone: 0,
        foot_push_speed: 0.0,
        feet_supported: [false; 2],
        foot_surface: 0,
        contact_count: 4,
        powersliding: false,
        trick_id: None,
        trick_identifier: None,
        animation_name: None,
        riding_switch: false,
        riding_fakie: false,
        nollie: false,
        wheel_surface: 3,
        events: Vec::new(),
        retail,
    }
}

#[test]
#[ignore = "needs the owner's assets; run explicitly"]
fn headless_rolling_through_the_retail_path() {
    use skate_audio_core::authored::{PCM_CHANNELS, PCM_FRAMES_PER_BLOCK};
    let assets = std::env::var_os("SKATE_ASSETS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| DEFAULT_ASSETS.into());
    let started = std::time::Instant::now();
    let super::Prepared {
        mut runtime,
        mut sound,
        ..
    } = super::prepare(&assets).expect("prepare the retail player-sound worker");
    eprintln!("prepared in {:.1} s", started.elapsed().as_secs_f32());

    // (seconds, speed): idle, slow, cruise, fast, stop.
    let script: [(u32, f32); 5] = [(2, 0.0), (3, 2.0), (3, 5.0), (3, 9.0), (2, 0.0)];
    let mut wav: Vec<f32> = Vec::new();
    let mut tick = 0u64;
    let mut blocks_owed = 0.0f64;
    let blocks_per_frame = 48_000.0 / f64::from(PCM_FRAMES_PER_BLOCK) / 60.0;
    let mut worst_ms = 0.0f64;
    for (seconds, speed) in script {
        let (mut peak, mut energy, mut samples, mut silent, mut blocks) = (0f32, 0f64, 0usize, 0, 0);
        for _ in 0..seconds * 60 {
            sound
                .frame(&mut runtime, &rolling(tick, speed))
                .expect("player-sound frame");
            tick += 1;
            blocks_owed += blocks_per_frame;
            while blocks_owed >= 1.0 {
                blocks_owed -= 1.0;
                let begin = std::time::Instant::now();
                let native = runtime.pump_once().expect("render one block");
                worst_ms = worst_ms.max(begin.elapsed().as_secs_f64() * 1e3);
                assert_eq!(
                    native.len(),
                    PCM_FRAMES_PER_BLOCK as usize * usize::from(PCM_CHANNELS)
                );
                let stereo = super::downmix(&native);
                let block_peak = stereo.iter().fold(0f32, |p, s| p.max(s.abs()));
                peak = peak.max(block_peak);
                energy += stereo.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>();
                samples += stereo.len();
                // Retail fades rolling in over a few frames; judge continuity after 0.25 s.
                if block_peak < 1.0e-5 && blocks >= 47 {
                    silent += 1;
                }
                blocks += 1;
                wav.extend_from_slice(&stereo);
            }
        }
        let stats = runtime.stats();
        if let Some((ctrl, c)) = sound.controls("SkateBoard") {
            use super::components::Controls;
            let levels: Vec<u32> = (0..20).map(|id| c.level(id)).collect();
            let raw: Vec<u32> = (0..20).map(|id| c.raw(id)).collect();
            let inputs: Vec<u32> = (0..16).map(|id| runtime.mixmap_get(ctrl, id).unwrap_or(0)).collect();
            eprintln!("  SkateBoard {ctrl:#010x} levels {levels:?}
  raw {raw:?}
  inputs {inputs:?}");
        }
        eprintln!(
            "speed {speed:>4.1} m/s for {seconds} s: peak {:>6.1} dBFS, rms {:>6.1} dBFS, silent blocks {silent}/{blocks}, voices opened {} live {}",
            20.0 * peak.max(1e-9).log10(),
            10.0 * (energy / samples.max(1) as f64).max(1e-18).log10(),
            stats.voices_opened,
            stats.live_voices,
        );
        if speed > 1.0 {
            assert_eq!(silent, 0, "rolling at {speed} m/s must never drop to silence");
        }
    }
    eprintln!("worst block render {worst_ms:.2} ms (budget 5.33 ms)");
    if let Some(path) = std::env::var_os("SKATE_HEADLESS_WAV") {
        write_wav(std::path::Path::new(&path), &wav).expect("write wav");
        eprintln!("wrote {}", std::path::Path::new(&path).display());
    }
}

/// Airborne: no wheels down, in the air state, with a hop's air timing.
fn airborne(tick: u64, speed: f32, elapsed: f32, jump_height: f32) -> PlayerAudioObservation {
    let mut observation = rolling(tick, speed);
    observation.state = 201;
    observation.grounded = false;
    observation.contact_count = 0;
    observation.retail.state = 201;
    observation.retail.state_category = 200;
    observation.retail.wheel_count = 0;
    observation.retail.wheel_contacts = [false; 4];
    observation.retail.truck_contacts = [false; 2];
    observation.retail.in_known_air = true;
    observation.retail.air_time_in_state = elapsed;
    observation.retail.air_time_until_landing = (0.7 - elapsed).max(0.0);
    observation.retail.air_jump_height = jump_height;
    observation
}

/// A hop and its landing must make the contact one-shots (`sub_824B9CC8` pops, `sub_824BA630`
/// landing impact) audible: they are bank voices from `Skate_Collisions.bnk`, not authored patches.
#[test]
#[ignore = "needs the owner's assets; run explicitly"]
fn headless_landing_impact_is_audible() {
    use skate_audio_core::authored::{PCM_CHANNELS, PCM_FRAMES_PER_BLOCK};
    let assets = std::env::var_os("SKATE_ASSETS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| DEFAULT_ASSETS.into());
    let super::Prepared { mut runtime, mut sound, .. } =
        super::prepare(&assets).expect("prepare the retail player-sound worker");
    let mut tick = 0u64;
    let mut blocks_owed = 0.0f64;
    let blocks_per_frame = 48_000.0 / f64::from(PCM_FRAMES_PER_BLOCK) / 60.0;
    // Roll, hop for 0.7 s, then land on all four and roll on.
    let script: Vec<PlayerAudioObservation> = (0..60)
        .map(|i| rolling(i, 5.0))
        .chain((0..42).map(|i| airborne(60 + i, 5.0, i as f32 / 60.0, 1.2)))
        .chain((0..120).map(|i| rolling(102 + i, 5.0)))
        .collect();
    let mut peaks = Vec::new();
    for observation in script {
        sound
            .frame(&mut runtime, &observation)
            .expect("player-sound frame");
        tick += 1;
        blocks_owed += blocks_per_frame;
        let mut frame_peak = 0.0f32;
        while blocks_owed >= 1.0 {
            blocks_owed -= 1.0;
            let native = runtime.pump_once().expect("render one block");
            assert_eq!(
                native.len(),
                PCM_FRAMES_PER_BLOCK as usize * usize::from(PCM_CHANNELS)
            );
            frame_peak = super::downmix(&native)
                .iter()
                .fold(frame_peak, |peak, sample| peak.max(sample.abs()));
        }
        peaks.push((tick, frame_peak));
    }
    // The airborne stretch is frames 61..102; the landing lands at 103.
    let air = peaks[70..100].iter().fold(0f32, |p, (_, v)| p.max(*v));
    let landing = peaks[102..130].iter().fold(0f32, |p, (_, v)| p.max(*v));
    eprintln!(
        "air peak {:.1} dBFS, landing peak {:.1} dBFS",
        20.0 * air.max(1e-9).log10(),
        20.0 * landing.max(1e-9).log10()
    );
    assert!(
        landing > air * 1.5,
        "the landing impact must be clearly louder than the airborne stretch: {landing} vs {air}"
    );
}

fn write_wav(path: &std::path::Path, stereo: &[f32]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data = (stereo.len() * 2) as u32;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&48_000u32.to_le_bytes())?;
    out.write_all(&(48_000u32 * 4).to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data.to_le_bytes())?;
    for sample in stereo {
        out.write_all(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    out.flush()
}
