//! Resolving a one-shot ("Splice") sound: which bank samples a sample id plays, and the vault
//! selection the wheel pops and the landing impact use.
//!
//! The bank format is `skate_audio_formats::splc`; the randomisation and the playback are
//! `skate_audio_core::authored::oneshot`. This module is the part between them: it walks a sample
//! id to its members exactly as `sub_82975700` and `sub_829757D0` do, in the retail draw order
//! (each child's probability first, then the container value, then each voice's pitch, gain and
//! delay), and it reads the pops and landing selection out of the vault.
//!
//! **Group state.** Retail keeps each group's and container's selection state in the bank's own
//! memory and mutates it in place (`sub_82976DD8`'s third argument). The banks here are placed in
//! guest memory read-only, so [`SpliceState`] holds those words host-side, keyed by the group's
//! offset in its bank. The effect is the same: the state persists per group for as long as the
//! bank is loaded.

use std::collections::HashMap;
use std::path::Path;

use skate_audio_core::authored::oneshot::{MemberValues, Rand, VoiceValues, container_value, pick, plays, voice_values};
use skate_audio_formats::splc::{Group, Resolved, Splc};
use skate_audio_formats::eb;

use super::Error;
use crate::collections::Collections;

/// The vault class the pops and the landing read (`[[0x830CFDA4]+24]`).
pub const CONTACTS_CLASS: &str = "Hash_C26949FCB638A2CA";
/// `Sk8::Audio::eEQChain`.
pub const EQ_CLASS: &str = "Hash_42AFE160E647167C";
/// The bank the pops and the landing name through the vault field types.
pub const COLLISIONS_BANK: &str = "Skate_Collisions.bnk";
/// The pops' DLC bank, absent from a base installation.
pub const DLC_COLLISIONS_BANK: &str = "DLC_Cartoon_Collisions.bnk";

/// One member a sample id resolved to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMember {
    /// The record the id resolved to.
    pub record: usize,
    /// Index into the bank's sample table.
    pub sample: u16,
    /// The sample's EAAC header, as an offset into the bank file: add the installed bank's base for
    /// the address the play command takes.
    pub stream_offset: u32,
    pub values: VoiceValues,
}

/// The selection state of every group and container, by bank name and offset.
#[derive(Clone, Debug, Default)]
pub struct SpliceState(HashMap<(String, usize), u32>);

impl SpliceState {
    fn at(&mut self, bank: &str, offset: usize) -> &mut u32 {
        self.0.entry((bank.to_owned(), offset)).or_insert(0)
    }
}

/// The parsed `SPLC` banks a one-shot can play from.
#[derive(Debug)]
pub struct SpliceBanks {
    banks: Vec<(String, Vec<u8>, Splc)>,
}

impl SpliceBanks {
    /// Parse the named banks out of `audiofiles.big`; a name the archive does not hold is skipped
    /// when `required` does not list it.
    pub fn load(archive: &Path, names: &[&str], required: &[&str]) -> Result<Self, Error> {
        let data = std::fs::read(archive)
            .map_err(|e| Error::Format(format!("{}: {e}", archive.display())))?;
        let parsed = eb::Archive::parse(&data)?;
        let mut banks = Vec::new();
        for &name in names {
            let member = parsed.entries.iter().find(|e| {
                e.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            });
            let Some(member) = member else {
                if required.contains(&name) {
                    return Err(Error::Format(format!("missing sound bank {name}")));
                }
                continue;
            };
            let range = member.range();
            let bytes = data
                .get(range)
                .ok_or_else(|| Error::Format(format!("{name}: member out of bounds")))?
                .to_vec();
            let bank = Splc::parse(&bytes)?;
            banks.push((name.to_owned(), bytes, bank));
        }
        Ok(Self { banks })
    }

    pub fn bank(&self, name: &str) -> Option<&Splc> {
        self.banks
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, _, bank)| bank)
    }

    fn entry(&self, name: &str) -> Result<(&str, &[u8], &Splc), Error> {
        self.banks
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            .map(|(n, bytes, bank)| (n.as_str(), bytes.as_slice(), bank))
            .ok_or_else(|| Error::Format(format!("sound bank {name} is not loaded")))
    }

    /// `sub_82975700` then `sub_829757D0`: the members a sample id plays, with the values
    /// `sub_82975A60`/`sub_82975CC8` give them. Members whose probability draw fails are left out,
    /// exactly as the retail walk skips them.
    pub fn resolve(
        &self,
        bank_name: &str,
        sample_id: u16,
        state: &mut SpliceState,
        rand: &mut Rand,
    ) -> Result<Vec<ResolvedMember>, Error> {
        let (name, bytes, bank) = self.entry(bank_name)?;
        let record = match bank.resolve(sample_id) {
            Resolved::Record(index) => index,
            Resolved::Container(index) => {
                let container = &bank.containers[index];
                let count = container.ids.len() as u8;
                let offset = 60 + 36 * bank.records.len() + 72 * index;
                let choice = pick(container.mode, count, state.at(name, offset), rand);
                let id = *container
                    .ids
                    .get(usize::from(choice))
                    .ok_or_else(|| Error::Format(format!("{name}: empty container {index}")))?;
                let index = usize::from(id);
                if index >= bank.records.len() {
                    return Err(Error::Format(format!(
                        "{name}: container id {id} is not a record"
                    )));
                }
                index
            }
        };

        // First pass, `sub_829757D0`: one probability draw per child, and the member it picked.
        let groups: Vec<Group> = bank.groups_of(record).to_vec();
        let mut chosen = Vec::new();
        for group in &groups {
            let choice = pick(group.mode, group.count, state.at(name, group.members_offset), rand);
            let member = bank.member(bytes, group, choice)?;
            let values = MemberValues {
                gain: member.gain,
                gain_range: member.gain_range,
                delay: member.delay,
                delay_range: member.delay_range,
                pitch_spread: member.pitch_spread,
                probability: member.probability,
            };
            if plays(&values, rand) {
                chosen.push((member.sample, values));
            }
        }
        // `sub_82975A60` draws the container value before the voices' own values.
        let _ = container_value(
            bank.records[record].value_base,
            bank.records[record].value_range,
            rand,
        );
        let mut out = Vec::with_capacity(chosen.len());
        for (sample, values) in chosen {
            let stream_offset = bank
                .stream_offset(sample)
                .ok_or_else(|| Error::Format(format!("{name}: sample {sample} is not in the table")))?;
            out.push(ResolvedMember {
                record,
                sample,
                stream_offset: stream_offset as u32,
                values: voice_values(&values, rand),
            });
        }
        Ok(out)
    }
}

/// The vault values the wheel pops read (class `0xC26949FCB638A2CA`, key `default`).
#[derive(Clone, Debug, PartialEq)]
pub struct PopsTuning {
    /// `0xF2A1E273ABB8E9AB` (0.42) and `0xD7758385CDB8DC26` (0.25): the audio state's `+468`
    /// thresholds for classes 2 and 1.
    pub high: f32,
    pub low: f32,
    /// `0x3C1E3B965A93594A` (surface categories 0 and 1) and `0x84DFABF76D821DEC` (2 and 3), both
    /// `Skate_Collisions` sample ids, indexed by the class.
    pub samples: [Vec<u16>; 2],
    /// `0x056E414053859F6E`, the `DLC_Cartoon_Collisions` ids of the DLC variant.
    pub dlc_samples: Vec<u16>,
    /// `0xE34B48082B5BF185`, the eEQChain bus.
    pub bus: u32,
}

/// The vault value the landing impact reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LandingTuning {
    /// `0x633FA94E39C1AE8F` — `Skate_Collisions` sample `0x447`.
    pub sample: u16,
}

fn ids(vault: &Collections, class: &str, name: &str) -> Result<Vec<u16>, Error> {
    let field = vault
        .field(class, "default", name)
        .map_err(Error::Format)?;
    let array = field
        .array
        .as_ref()
        .ok_or_else(|| Error::Format(format!("{name} is not an array")))?;
    array
        .items
        .iter()
        .map(|item| {
            u32::from_str_radix(item.trim(), 16)
                .map(|value| value as u16)
                .map_err(|e| Error::Format(format!("{name}: {e}")))
        })
        .collect()
}

impl PopsTuning {
    pub fn load(vault: &Collections) -> Result<Self, Error> {
        let float = |name: &str| {
            vault
                .float(CONTACTS_CLASS, "default", name)
                .map_err(Error::Format)
        };
        Ok(Self {
            high: float("Hash_F2A1E273ABB8E9AB")?,
            low: float("Hash_D7758385CDB8DC26")?,
            samples: [
                ids(vault, CONTACTS_CLASS, "Hash_3C1E3B965A93594A")?,
                ids(vault, CONTACTS_CLASS, "Hash_84DFABF76D821DEC")?,
            ],
            dlc_samples: ids(vault, CONTACTS_CLASS, "Hash_056E414053859F6E")?,
            bus: vault
                .words::<1>(EQ_CLASS, "default", "Hash_E34B48082B5BF185")
                .map_err(Error::Format)?[0],
        })
    }

    /// `sub_824B9CC8`'s class: the audio state's `+468` against the two thresholds, with trick ids
    /// 33 and 34 forcing class 0.
    pub fn class(&self, jump_velocity_468: f32, trick: i32) -> usize {
        if trick == 33 || trick == 34 {
            return 0;
        }
        if jump_velocity_468 > self.high {
            2
        } else if jump_velocity_468 > self.low {
            1
        } else {
            0
        }
    }

    /// `sub_824B9AD8`: the bank and sample for a class and surface category. `dlc` is retail's
    /// special case (its global flag, a local player and player index 0), which plays the DLC bank's
    /// variant at bank index 8.
    pub fn sample(&self, class: usize, category: u8, dlc: bool) -> Option<(&'static str, u16)> {
        if dlc {
            return self
                .dlc_samples
                .get(class)
                .copied()
                .map(|id| (DLC_COLLISIONS_BANK, id));
        }
        let table = usize::from(category >= 2);
        self.samples[table]
            .get(class)
            .copied()
            .map(|id| (COLLISIONS_BANK, id))
    }
}

impl LandingTuning {
    pub fn load(vault: &Collections) -> Result<Self, Error> {
        Ok(Self {
            sample: vault
                .words::<1>(CONTACTS_CLASS, "default", "Hash_633FA94E39C1AE8F")
                .map_err(Error::Format)?[0] as u16,
        })
    }

    /// The landing impact plays on the default output bus (`sub_824BA630` passes
    /// `[[[0x830CFDBC]+44]]`).
    pub fn bank(&self) -> &'static str {
        COLLISIONS_BANK
    }
}

/// `sub_824BA310`: the pops' surface category — the wheel material's `AudioSurfaceMap` lane `+8`
/// (`sub_82494D78`) and the soft-wheel test (`sub_824B23C8`).
pub fn surface_category(map_lane_8: u32, soft_wheels: bool) -> u8 {
    u8::from(soft_wheels) + if map_lane_8 != 0 { 2 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_category_combines_the_surface_lane_and_the_wheels() {
        assert_eq!(surface_category(0, false), 0);
        assert_eq!(surface_category(0, true), 1);
        assert_eq!(surface_category(1, false), 2);
        assert_eq!(surface_category(7, true), 3);
    }

    fn tuning() -> PopsTuning {
        // The owner's vault values.
        PopsTuning {
            high: 0.42,
            low: 0.25,
            samples: [vec![0x449, 0x44A, 0x44B], vec![0x44F, 0x450, 0x451]],
            dlc_samples: vec![0x15, 0x16, 0x16],
            bus: 0,
        }
    }

    #[test]
    fn the_pop_class_follows_the_thresholds_and_the_trick() {
        let t = tuning();
        assert_eq!(t.class(0.0, -1), 0);
        assert_eq!(t.class(0.25, -1), 0);
        assert_eq!(t.class(0.30, -1), 1);
        assert_eq!(t.class(0.42, -1), 1);
        assert_eq!(t.class(0.50, -1), 2);
        // Tricks 33 and 34 always take the quietest pop.
        assert_eq!(t.class(0.99, 33), 0);
        assert_eq!(t.class(0.99, 34), 0);
        assert_eq!(t.class(0.99, 35), 2);
    }

    #[test]
    fn the_sample_follows_the_class_the_category_and_the_dlc_case() {
        let t = tuning();
        assert_eq!(t.sample(0, 0, false), Some((COLLISIONS_BANK, 0x449)));
        assert_eq!(t.sample(2, 1, false), Some((COLLISIONS_BANK, 0x44B)));
        assert_eq!(t.sample(0, 2, false), Some((COLLISIONS_BANK, 0x44F)));
        assert_eq!(t.sample(1, 3, false), Some((COLLISIONS_BANK, 0x450)));
        assert_eq!(t.sample(1, 0, true), Some((DLC_COLLISIONS_BANK, 0x16)));
    }

    /// The vault must hold what the selection expects, including the field types that name the
    /// banks.
    #[test]
    #[ignore = "needs the installed assets"]
    fn the_vault_holds_the_pops_and_landing_selection() {
        let assets =
            std::path::PathBuf::from(r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets");
        let Ok(vault) = Collections::load(&assets) else { return };
        let pops = PopsTuning::load(&vault).unwrap();
        let landing = LandingTuning::load(&vault).unwrap();
        println!("pops {pops:?}\nlanding {landing:?}");
        assert_eq!(pops, tuning());
        assert_eq!(landing.sample, 0x447);
        for name in [
            "Hash_3C1E3B965A93594A",
            "Hash_84DFABF76D821DEC",
            "Hash_633FA94E39C1AE8F",
        ] {
            let field = vault.field(CONTACTS_CLASS, "default", name).unwrap();
            assert_eq!(field.type_name, "Skate_Collisions", "{name} names its bank");
        }
        let dlc = vault
            .field(CONTACTS_CLASS, "default", "Hash_056E414053859F6E")
            .unwrap();
        assert_eq!(dlc.type_name, "DLC_Cartoon_Collisions");
    }

    /// Resolving the pops' and the landing's ids against the real bank.
    #[test]
    #[ignore = "needs the installed assets"]
    fn resolves_the_pops_and_landing_samples() {
        let assets =
            std::path::PathBuf::from(r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets");
        let archive = assets.join("private/stock/data/audio/audiofiles.big");
        let Ok(banks) = SpliceBanks::load(&archive, &[COLLISIONS_BANK], &[COLLISIONS_BANK]) else {
            return;
        };
        let bank = banks.bank(COLLISIONS_BANK).unwrap();
        println!(
            "{}: {} records, {} containers, {} samples",
            bank.name,
            bank.records.len(),
            bank.containers.len(),
            bank.samples.len()
        );
        let mut state = SpliceState::default();
        let mut rand = Rand::new(1);
        for id in [0x447u16, 0x449, 0x44A, 0x44B, 0x44F, 0x450, 0x451] {
            let mut samples = std::collections::BTreeSet::new();
            for _ in 0..16 {
                let voices = banks
                    .resolve(COLLISIONS_BANK, id, &mut state, &mut rand)
                    .unwrap();
                assert!(!voices.is_empty(), "id {id:#x} resolved to nothing");
                for voice in &voices {
                    assert!(voice.values.gain > 0.0, "id {id:#x} gain {}", voice.values.gain);
                    assert!(voice.values.pitch > 0.0);
                    assert!(
                        usize::from(voice.sample) < bank.samples.len(),
                        "id {id:#x} sample {}",
                        voice.sample
                    );
                    samples.insert(voice.sample);
                }
            }
            println!("  {id:#x} -> samples {samples:?}");
        }
    }

    /// Plays a pop and a landing on the real runtime: the chosen samples must be the vault's, the
    /// output must be audible and sane, and every voice must be gone afterwards.
    #[test]
    #[ignore = "needs the installed assets"]
    fn plays_a_pop_and_a_landing_headlessly() {
        use skate_audio_core::authored::AuthoredRuntime;
        use skate_audio_core::authored::oneshot::{OneshotBus, OneshotVoice};
        let assets =
            std::path::PathBuf::from(r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets");
        let archive = assets.join("private/stock/data/audio/audiofiles.big");
        if !archive.exists() {
            return;
        }
        let cache = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Skate3RustEngine/audio-pcm-cache"));
        let mut catalog =
            super::super::catalog::PlayerAudioCatalog::from_assets(&assets, cache.as_deref()).unwrap();
        catalog.load_splice_banks(&archive, cache.as_deref()).unwrap();
        let splice_samples = catalog
            .samples
            .iter()
            .filter(|s| s.bank.eq_ignore_ascii_case(COLLISIONS_BANK))
            .count();
        assert!(splice_samples > 1000, "only {splice_samples} collision samples decoded");
        let mut runtime =
            AuthoredRuntime::new(catalog.guest, catalog.projects, catalog.banks).unwrap();
        for sample in catalog.samples {
            let base = runtime.bank_base(&sample.bank).unwrap();
            runtime.insert_pcm(base + sample.header_offset, sample.pcm).unwrap();
        }
        let base = runtime.bank_base(COLLISIONS_BANK).unwrap();
        let banks = SpliceBanks::load(&archive, &[COLLISIONS_BANK], &[COLLISIONS_BANK]).unwrap();
        let vault = Collections::load(&assets).unwrap();
        let pops = PopsTuning::load(&vault).unwrap();
        let landing = LandingTuning::load(&vault).unwrap();
        let mut state = SpliceState::default();
        let mut rand = Rand::new(1);

        let mut play = |runtime: &mut AuthoredRuntime,
                        state: &mut SpliceState,
                        rand: &mut Rand,
                        sample_id: u16,
                        bus: OneshotBus| {
            let voices = banks
                .resolve(COLLISIONS_BANK, sample_id, state, rand)
                .unwrap();
            assert!(!voices.is_empty());
            let chosen: Vec<u16> = voices.iter().map(|v| v.sample).collect();
            let handles: Vec<_> = voices
                .iter()
                .map(|member| {
                    runtime
                        .play_oneshot(&OneshotVoice {
                            sample: base + member.stream_offset,
                            gain: member.values.gain.min(1.0),
                            pitch: member.values.pitch,
                            delay: member.values.delay,
                            bus,
                        })
                        .unwrap()
                })
                .collect();
            (chosen, handles)
        };

        // A hard pop (class 2 at +468 = 0.5) on a plain surface, on the pops' eEQChain bus.
        let class = pops.class(0.5, -1);
        assert_eq!(class, 2);
        let (_, sample_id) = pops.sample(class, surface_category(0, false), false).unwrap();
        assert_eq!(sample_id, 0x44B);
        // The runtime's own bus and output players only appear in the stats once a block has been
        // pumped, so the baseline is taken after one.
        runtime.pump_once().unwrap();
        let before = runtime.stats().live_voices;
        let (pop_samples, mut handles) =
            play(&mut runtime, &mut state, &mut rand, sample_id, OneshotBus::EqChain(pops.bus));
        // The landing impact, on the default output bus.
        let (land_samples, land_handles) = play(
            &mut runtime,
            &mut state,
            &mut rand,
            landing.sample,
            OneshotBus::Default,
        );
        handles.extend(land_handles);
        println!("pop samples {pop_samples:?}, landing samples {land_samples:?}");

        // Half a second of blocks, ticking the one-shots at 60 Hz as a component would.
        let mut peak = 0.0f32;
        let mut rms = 0.0f64;
        let mut frames = 0usize;
        let mut blocks = 0.0f64;
        let mut live_peak = 0usize;
        for _ in 0..30 {
            for handle in handles.iter_mut() {
                runtime.tick_oneshot(handle, 1.0 / 60.0).unwrap();
            }
            blocks += 48_000.0 / 256.0 / 60.0;
            while blocks >= 1.0 {
                blocks -= 1.0;
                let pcm = runtime.pump_once().unwrap();
                for s in &pcm {
                    peak = peak.max(s.abs());
                    rms += f64::from(*s) * f64::from(*s);
                }
                frames += pcm.len();
            }
            live_peak = live_peak.max(runtime.stats().live_voices);
        }
        let rms = (rms / frames.max(1) as f64).sqrt();
        println!(
            "peak {peak:.4} rms {rms:.5} live voices before {before}, peak {live_peak}, after {}",
            runtime.stats().live_voices
        );
        assert!(peak > 1e-3, "the one-shots are silent (peak {peak})");
        assert!(peak <= 1.5, "the one-shots clip hard (peak {peak})");
        assert!(live_peak > before, "no voice was opened");

        // Every voice must retire: tick until the handles report finished, then the live count must
        // be back where it started.
        for _ in 0..600 {
            let mut live = false;
            for handle in handles.iter_mut() {
                live |= runtime.tick_oneshot(handle, 1.0 / 60.0).unwrap();
            }
            runtime.pump_once().unwrap();
            if !live {
                break;
            }
        }
        let after = runtime.stats().live_voices;
        println!("live voices after the one-shots finished: {after}");
        assert!(
            handles.iter().all(|h| h.voice == 0),
            "a one-shot was never released"
        );
        assert_eq!(after, before, "the one-shots leaked voices");
    }
}
