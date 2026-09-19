//! The SenseOfSpeed component (vtable 0x822FCF08, SenseOfSpeed controller 40010080):
//! SenseOfSpeed_rattle (holder `+36`) and SenseOfSpeed_wind (holder `+40`).
//!
//! - Process `sub_824E7980` (slot 9), gated on the owner's local byte `[this+28]+72`:
//!   rattle posts (`sub_824B0520`, 48-byte object, 11 words) while the board ground speed
//!   `+208 × 3.6 ≥ low` and is released below it; wind posts (`sub_824B0388`, 56-byte object,
//!   13 words) while the COM speed `+212 × 3.6 ≥ low` and is released below it, with the bail
//!   pair of bounds while `+676`. Intensity = `fctiwz(clamp01((kmh − low) / (high − low)) × 1000)`.
//! - Update `sub_824E7CB0` (slot 10), same gate: rattle w0 (vault), w1 vf52(0), w2 vf56(2),
//!   w6 0, w7 vf60(1), w3; wind w0 (vault), w2 vf56(3), w6 0, w7 vf60(4), w3. The updater's
//!   intensity uses `fmsubs` (`v × 3.6 − low` fused, one rounding).
//!
//! Not ported (out of scope): the `x_jet_rolling.grain` "AudBoard Rocket" grain player at `+100`
//! (`+128` playing flag, started above `+136` = 35 km/h by `sub_828EC040`/`sub_828ECAB0`/
//! `sub_828EC3F0`, stopped by `sub_828EBF90`, fed by the updater's second block with vf60(5),
//! vf56(3) and `+132`).
//!
//! Tuning: `sub_824E80D0` reads holder `*(0x830CFDA4)+132` = class `6E878344774A7999`/`default`;
//! eEQChain from holder `+140` = `42AFE160E647167C`/`default`.

use skate_data::collections::Collections;

use super::super::audio_state::AudioState;
use super::contacts::{clamp_word, vault_word};
use super::words::{fctiwz, KMH_PER_MS, THOUSAND};
use super::{post, redeliver, release, Component, Controls, Tick};

const TUNING_CLASS: &str = "Hash_6E878344774A7999";
const DEFAULT_KEY: &str = "Hash_D7EDBD362D7D2152";
const EQ_CLASS: &str = "Hash_42AFE160E647167C";

/// Packet lengths: object sizes minus the 4-byte header.
pub(crate) const RATTLE_WORDS: usize = 11;
pub(crate) const WIND_WORDS: usize = 13;
const RATTLE_OBJECT: &str = "SenseOfSpeed_rattle";
const WIND_OBJECT: &str = "SenseOfSpeed_wind";
/// `0x8231A844` (1.0), `0x82165A10` (0.0).
const ONE: f32 = f32::from_bits(0x3F80_0000);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpeedTuning {
    /// `+44` `EB224272A924C135` (rattle w0), `+48` `1BBA9174BD1B2D88` (wind w0).
    pub rattle_level: i32,
    pub wind_level: i32,
    /// `+52`/`+56` rattle bounds km/h `F57A74AFD22AD030` / `12275AA8AC4A63FB`.
    pub rattle_kmh: [f32; 2],
    /// `+60`/`+64` wind bounds `8B4DECD646B13210` / `73E9A42D88C51169`; `+68`/`+72` while
    /// bailing `3BDFAC298128131F` / `7BCB09DF227EDDAC`.
    pub wind_kmh: [f32; 2],
    pub bail_kmh: [f32; 2],
    /// Wind constructor w9..w12: `+76` `4381FEC9776F5EC9`, `+80` `F7A8067CAE4C51D3`, `+84`
    /// `D2715B3AD1BBCDB5`, `+88` `C689DD3434BC4170`.
    pub wind_words: [i32; 4],
    /// Rattle constructor w9, w10: `+92` `75F14A57A506E686`, `+96` `5F0E3DDE232C2BF3`.
    pub rattle_words: [i32; 2],
    /// eEQChain `4D30A6CA8C9E1ABC` (rattle w8), `C1DC8556BA66CDCD` (wind w8).
    pub rattle_eq: i32,
    pub wind_eq: i32,
}

impl SpeedTuning {
    pub(crate) fn load(vault: &Collections) -> Result<Self, String> {
        let int = |name: &str| vault.integer(TUNING_CLASS, DEFAULT_KEY, name).map(|v| v as i32);
        let float = |name: &str| vault.float(TUNING_CLASS, DEFAULT_KEY, name);
        Ok(Self {
            rattle_level: int("Hash_EB224272A924C135")?,
            wind_level: int("Hash_1BBA9174BD1B2D88")?,
            rattle_kmh: [float("Hash_F57A74AFD22AD030")?, float("Hash_12275AA8AC4A63FB")?],
            wind_kmh: [float("Hash_8B4DECD646B13210")?, float("Hash_73E9A42D88C51169")?],
            bail_kmh: [float("Hash_3BDFAC298128131F")?, float("Hash_7BCB09DF227EDDAC")?],
            wind_words: [
                int("Hash_4381FEC9776F5EC9")?,
                int("Hash_F7A8067CAE4C51D3")?,
                int("Hash_D2715B3AD1BBCDB5")?,
                int("Hash_C689DD3434BC4170")?,
            ],
            rattle_words: [int("Hash_75F14A57A506E686")?, int("Hash_5F0E3DDE232C2BF3")?],
            rattle_eq: vault_word(vault, EQ_CLASS, DEFAULT_KEY, "Hash_4D30A6CA8C9E1ABC")? as i32,
            wind_eq: vault_word(vault, EQ_CLASS, DEFAULT_KEY, "Hash_C1DC8556BA66CDCD")? as i32,
        })
    }

    /// The wind bounds: the bail pair while `+676`.
    fn wind_bounds(&self, audio: &AudioState) -> [f32; 2] {
        if audio.bail_676 { self.bail_kmh } else { self.wind_kmh }
    }
}

/// `fsel` clamp to 0..1 then `× 1000`, `fctiwz`.
fn intensity_word(offset: f32, span: f32) -> i32 {
    let x = offset / span;
    let low = if -x >= 0.0 { 0.0 } else { x };
    let unit = if ONE - low >= 0.0 { low } else { ONE };
    fctiwz(unit * THOUSAND)
}

/// The process's intensity: `kmh` already rounded (`fmuls`), then `fsubs`, `fsubs`, `fdivs`.
pub(crate) fn process_intensity(kmh: f32, bounds: [f32; 2]) -> i32 {
    intensity_word(kmh - bounds[0], bounds[1] - bounds[0])
}

/// The updater's intensity: `fmsubs(speed, 3.6, low)` (fused), `fsubs`, `fdivs`.
pub(crate) fn update_intensity(speed: f32, bounds: [f32; 2]) -> i32 {
    intensity_word(speed.mul_add(KMH_PER_MS, -bounds[0]), bounds[1] - bounds[0])
}

/// `sub_824B0520`.
pub(crate) fn rattle_constructor(tuning: &SpeedTuning, intensity: i32) -> [u32; RATTLE_WORDS] {
    let mut words = [0; RATTLE_WORDS];
    words[2] = 4_096;
    words[3] = clamp_word(intensity, 0, 1_000);
    words[4] = 25_000;
    words[8] = clamp_word(tuning.rattle_eq, 0, 32_767);
    words[9] = clamp_word(tuning.rattle_words[0], 0, 32_767);
    words[10] = clamp_word(tuning.rattle_words[1], 0, 32_767);
    words
}

/// `sub_824B0388`.
pub(crate) fn wind_constructor(tuning: &SpeedTuning, intensity: i32) -> [u32; WIND_WORDS] {
    let mut words = [0; WIND_WORDS];
    words[2] = 4_096;
    words[3] = clamp_word(intensity, 0, 1_000);
    words[4] = 25_000;
    words[8] = clamp_word(tuning.wind_eq, 0, 32_767);
    for (i, level) in tuning.wind_words.iter().enumerate() {
        words[9 + i] = clamp_word(*level, 0, 32_767);
    }
    words
}

/// `sub_824E7CB0`'s rattle rewrite.
pub(crate) fn rattle_update(words: &mut [u32; RATTLE_WORDS], tuning: &SpeedTuning, audio: &AudioState, controls: &dyn Controls) {
    words[0] = clamp_word(tuning.rattle_level, 0, 32_767);
    words[1] = clamp_word(controls.raw(0) as i32, 0, 0xFFFF);
    words[2] = clamp_word(controls.pitch(2), 0, 8_192);
    words[6] = 0;
    words[7] = clamp_word(controls.level(1) as i32, 0, 32_767);
    words[3] = clamp_word(update_intensity(audio.ground_speed_208, tuning.rattle_kmh), 0, 1_000);
}

/// `sub_824E7CB0`'s wind rewrite (w1 keeps the constructor's 0).
pub(crate) fn wind_update(words: &mut [u32; WIND_WORDS], tuning: &SpeedTuning, audio: &AudioState, controls: &dyn Controls) {
    words[0] = clamp_word(tuning.wind_level, 0, 32_767);
    words[2] = clamp_word(controls.pitch(3), 0, 8_192);
    words[6] = 0;
    words[7] = clamp_word(controls.level(4) as i32, 0, 32_767);
    words[3] = clamp_word(update_intensity(audio.com_speed_212, tuning.wind_bounds(audio)), 0, 1_000);
}

/// What one process call did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SpeedEvents {
    pub post_rattle: Option<[u32; RATTLE_WORDS]>,
    pub post_wind: Option<[u32; WIND_WORDS]>,
    pub release_rattle: bool,
    pub release_wind: bool,
}

pub(crate) struct SenseOfSpeed {
    tuning: SpeedTuning,
    /// Owner byte `[this+28]+72`: the engine drives the local player only.
    local_72: bool,
    rattle: Option<(u32, [u32; RATTLE_WORDS])>,
    wind: Option<(u32, [u32; WIND_WORDS])>,
}

impl SenseOfSpeed {
    pub(crate) fn new(vault: &Collections) -> Result<Self, String> {
        Ok(Self::with_tuning(SpeedTuning::load(vault)?))
    }

    fn with_tuning(tuning: SpeedTuning) -> Self {
        Self { tuning, local_72: true, rattle: None, wind: None }
    }

    /// `sub_824E7980` (without the rocket grain).
    pub(crate) fn step_process(&self, audio: &AudioState) -> SpeedEvents {
        let mut events = SpeedEvents::default();
        if !self.local_72 {
            return events;
        }
        let kmh = audio.ground_speed_208 * KMH_PER_MS;
        if kmh >= self.tuning.rattle_kmh[0] {
            if self.rattle.is_none() {
                let intensity = process_intensity(kmh, self.tuning.rattle_kmh);
                events.post_rattle = Some(rattle_constructor(&self.tuning, intensity));
            }
        } else {
            events.release_rattle = self.rattle.is_some();
        }
        let bounds = self.tuning.wind_bounds(audio);
        let kmh = audio.com_speed_212 * KMH_PER_MS;
        if kmh >= bounds[0] {
            if self.wind.is_none() {
                events.post_wind = Some(wind_constructor(&self.tuning, process_intensity(kmh, bounds)));
            }
        } else {
            events.release_wind = self.wind.is_some();
        }
        events
    }

    /// `sub_824E7CB0` (without the rocket grain): rewrite what is held.
    pub(crate) fn rewrite(&mut self, audio: &AudioState, controls: &dyn Controls) {
        if !self.local_72 {
            return;
        }
        if let Some((_, words)) = self.rattle.as_mut() {
            rattle_update(words, &self.tuning, audio, controls);
        }
        if let Some((_, words)) = self.wind.as_mut() {
            wind_update(words, &self.tuning, audio, controls);
        }
    }

    #[cfg(test)]
    fn apply_local(&mut self, events: &SpeedEvents) {
        if events.release_rattle {
            self.rattle = None;
        }
        if events.release_wind {
            self.wind = None;
        }
        if let Some(words) = events.post_rattle {
            self.rattle = Some((0, words));
        }
        if let Some(words) = events.post_wind {
            self.wind = Some((0, words));
        }
    }
}

impl Component for SenseOfSpeed {
    fn process(&mut self, tick: &mut Tick) -> Result<(), String> {
        let events = self.step_process(tick.audio);
        if events.release_rattle {
            let mut handle = self.rattle.take().map(|(handle, _)| handle);
            release(tick.runtime, &mut handle)?;
        }
        if let Some(words) = events.post_rattle {
            self.rattle = Some((post(tick.runtime, RATTLE_OBJECT, &words)?, words));
        }
        if events.release_wind {
            let mut handle = self.wind.take().map(|(handle, _)| handle);
            release(tick.runtime, &mut handle)?;
        }
        if let Some(words) = events.post_wind {
            self.wind = Some((post(tick.runtime, WIND_OBJECT, &words)?, words));
        }
        Ok(())
    }

    fn update(&mut self, tick: &mut Tick) -> Result<(), String> {
        self.rewrite(tick.audio, tick.controls);
        if let Some((handle, words)) = self.rattle.as_ref() {
            redeliver(tick.runtime, *handle, words)?;
        }
        if let Some((handle, words)) = self.wind.as_ref() {
            redeliver(tick.runtime, *handle, words)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::contacts::capture::{self, Captured, Matches};
    use super::*;

    fn tuning() -> SpeedTuning {
        SpeedTuning {
            rattle_level: 32_767,
            wind_level: 27_000,
            rattle_kmh: [30.0, 80.0],
            wind_kmh: [15.0, 55.0],
            bail_kmh: [1.0, 10.0],
            wind_words: [32_767, 17_000, 17_000, 6_750],
            rattle_words: [32_767, 23_000],
            rattle_eq: 7,
            wind_eq: 7,
        }
    }

    #[test]
    fn constructors_match_retail_posts() {
        // Wind post, frame 2708 (w3 13); rattle post, frame 3754 (w3 8).
        assert_eq!(
            wind_constructor(&tuning(), 13),
            [0, 0, 4096, 13, 25000, 0, 0, 0, 7, 32767, 17000, 17000, 6750]
        );
        assert_eq!(rattle_constructor(&tuning(), 8), [0, 0, 4096, 8, 25000, 0, 0, 0, 7, 32767, 23000]);
    }

    #[test]
    fn posts_and_releases_at_the_low_bound_and_bail_switches_the_wind_pair() {
        let mut sos = SenseOfSpeed::with_tuning(tuning());
        // 20 km/h COM, 10 km/h ground: wind only.
        let audio = { let mut s = AudioState::default(); s.com_speed_212 = 20.0 / 3.6; s.ground_speed_208 = 10.0 / 3.6; s };
        let events = sos.step_process(&audio);
        assert!(events.post_wind.is_some() && events.post_rattle.is_none());
        sos.apply_local(&events);
        // Slow COM releases wind, unless bailing (1..10 km/h).
        let slow = { let mut s = AudioState::default(); s.com_speed_212 = 5.0 / 3.6; s };
        assert!(sos.step_process(&slow).release_wind);
        let bail = { let mut s = slow.clone(); s.bail_676 = true; s };
        assert!(!sos.step_process(&bail).release_wind);
        let mut words = sos.wind.unwrap().1;
        wind_update(&mut words, &tuning(), &bail, &Captured::default());
        // (5 − 1) / (10 − 1) → 444.
        assert_eq!(words[3], 444);
    }

    #[test]
    fn update_intensity_is_fused() {
        // Both roundings agree away from ties; the fused form never differs by more than a unit.
        let v = f32::from_bits(0x4105_5555);
        assert!((update_intensity(v, [30.0, 80.0]) - process_intensity(v * KMH_PER_MS, [30.0, 80.0])).abs() <= 1);
    }

    /// Replays the retail capture: posts/releases at process F (state F) and the updates of
    /// F+1 (state F).
    #[test]
    #[ignore = "needs the retail capture in .local"]
    fn sense_of_speed_replays_the_retail_capture() {
        let Some(root) = capture::root() else { return };
        let states = capture::states(&root);
        let group = |object: &str| {
            let mut map = std::collections::BTreeMap::<(u32, String), Vec<capture::Row>>::new();
            for row in capture::rows(&root, object) {
                map.entry((row.frame, row.kind.clone())).or_default().push(row);
            }
            map
        };
        let (rattle, wind) = (group("SenseOfSpeed_rattle"), group("SenseOfSpeed_wind"));
        let get = |map: &std::collections::BTreeMap<(u32, String), Vec<capture::Row>>, frame: u32, kind: &str| {
            map.get(&(frame, kind.to_string())).cloned().unwrap_or_default()
        };
        let mut sos = SenseOfSpeed::with_tuning(tuning());
        let mut rattle_up = Matches::new("SenseOfSpeed_rattle update", RATTLE_WORDS);
        let mut wind_up = Matches::new("SenseOfSpeed_wind update", WIND_WORDS);
        let mut rattle_po = Matches::new("SenseOfSpeed_rattle post", RATTLE_WORDS);
        let mut wind_po = Matches::new("SenseOfSpeed_wind post", WIND_WORDS);
        let (mut timing_ok, mut timing_bad) = (0, Vec::new());
        let (mut fused_diff, mut plain_diff) = (0, 0);
        let first = *states.keys().next().unwrap();
        let last = *states.keys().last().unwrap();
        for frame in first + 1..=last {
            if let Some(state) = states.get(&(frame - 1)) {
                let audio = AudioState::from_capture(state);
                let reads: Vec<_> = get(&rattle, frame, "UP")
                    .iter()
                    .chain(get(&wind, frame, "UP").iter())
                    .flat_map(|r| r.reads.clone())
                    .collect();
                sos.rewrite(&audio, &Captured::from_reads(&reads, 0x824E_7CB0..0x824E_80D0));
                for row in get(&rattle, frame, "UP") {
                    if let Some((_, words)) = sos.rattle.as_ref() {
                        rattle_up.add(frame, &row.words[..RATTLE_WORDS], words);
                        let plain = process_intensity(audio.ground_speed_208 * KMH_PER_MS, tuning().rattle_kmh).clamp(0, 1_000) as u32;
                        fused_diff += usize::from(words[3] != row.words[3]);
                        plain_diff += usize::from(plain != row.words[3]);
                    }
                }
                for row in get(&wind, frame, "UP") {
                    if let Some((_, words)) = sos.wind.as_ref() {
                        wind_up.add(frame, &row.words[..WIND_WORDS], words);
                    }
                }
            }
            if let Some(state) = states.get(&frame) {
                let audio = AudioState::from_capture(state);
                let events = sos.step_process(&audio);
                let (r_po, w_po) = (get(&rattle, frame, "PO"), get(&wind, frame, "PO"));
                let (r_rl, w_rl) = (get(&rattle, frame, "RL"), get(&wind, frame, "RL"));
                if let (Some(words), Some(row)) = (events.post_rattle, r_po.first()) {
                    rattle_po.add(frame, &row.words[..RATTLE_WORDS], &words);
                }
                if let (Some(words), Some(row)) = (events.post_wind, w_po.first()) {
                    wind_po.add(frame, &row.words[..WIND_WORDS], &words);
                }
                let ours = (events.post_rattle.is_some(), events.post_wind.is_some(), events.release_rattle, events.release_wind);
                let retail = (!r_po.is_empty(), !w_po.is_empty(), !r_rl.is_empty(), !w_rl.is_empty());
                if ours == retail {
                    timing_ok += 1;
                } else if timing_bad.len() < 12 {
                    timing_bad.push((frame, ours, retail));
                }
                sos.apply_local(&events);
            }
        }
        rattle_po.print();
        wind_po.print();
        rattle_up.print();
        wind_up.print();
        println!("rattle w3 mismatches: fused {fused_diff}, unfused {plain_diff}");
        println!("frames with matching post/release {timing_ok}; mismatches (frame, ours, retail) {timing_bad:?}");
    }
}
