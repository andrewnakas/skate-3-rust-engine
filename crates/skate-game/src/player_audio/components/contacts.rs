//! Class_foot_drag (Contacts controller 40010010).
//!
//! The Contacts component's process (`sub_824B8218`, gated on `[[this+16]+52]`) calls the trigger
//! `sub_824BB540`; its update (`sub_824BE130`, same gate) calls the updater `sub_824BEEE8`. The
//! held message lives at component `+128`; the constructor is `sub_824AF498` (64-byte object,
//! 15 words, message slot 0 = `0x8302EE28`).
//!
//! - Trigger: while `+336 || +339 || (local && +310)` and nothing is held, post
//!   w7 = speed (or 500 on the local hold path), w8 = foot surface (`sub_824BA390`), w9..w12 vault
//!   levels, w13 = `!+336`, w14 = vault eEQChain (the manual-brake one while `+339`).
//! - Updater: once none of the three holds, release; otherwise rewrite w0..w8 and redeliver.
//!
//! Every tuning value is read from the vault at construction through the audio tuning holder
//! `*(0x830CFDA4)`: +24 = class `C26949FCB638A2CA`/`default`, +64 = the AudioSurfaceMap
//! (`C1831BDB6CB1B1EA`/`C489459A0C07D154`, field `4CA607558B1CF440`), +140 = eEQChain class
//! `42AFE160E647167C`/`default`.

use skate_data::collections::Collections;

use super::super::audio_state::AudioState;
use super::words::{fctiwz, KMH_PER_MS, TEN_THOUSAND};
use super::{post, redeliver, release, Component, Controls, Tick};

/// Holder +24 (`sub_8279C948` class) and holder +140 (eEQChain) collections.
const TUNING_CLASS: &str = "Hash_C26949FCB638A2CA";
const EQ_CLASS: &str = "Hash_42AFE160E647167C";
const DEFAULT_KEY: &str = "Hash_D7EDBD362D7D2152";
/// Holder +64: `Sk8::AudioSurfaceMap`, 95 elements of 72 bytes.
const SURFACE_CLASS: &str = "Hash_C1831BDB6CB1B1EA";
const SURFACE_KEY: &str = "Hash_C489459A0C07D154";
const SURFACE_FIELD: &str = "Hash_4CA607558B1CF440";

/// `0x8209975C`: the updater's speed offset (the trigger reads the vault's instead).
const UPDATER_SPEED_OFFSET: f32 = f32::from_bits(0x3F00_0000);
/// Packet length: the constructor's 64-byte object minus the 4-byte header.
pub(crate) const FOOT_DRAG_WORDS: usize = 15;
const OBJECT: &str = "Class_foot_drag";

/// PowerPC `fsel`: `a >= 0 ? b : c` (NaN selects `c`).
pub(crate) fn fsel(a: f32, b: f32, c: f32) -> f32 {
    if a >= 0.0 { b } else { c }
}

/// The updaters' `cmpwi`/`li` clamp of a signed word.
pub(crate) fn clamp_word(value: i32, low: i32, high: i32) -> u32 {
    (if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }) as u32
}

/// The first 32-bit lane of a vault field, whatever its reflection type (eEQChain enums are
/// 32-bit values the code reads with `lwz`).
pub(crate) fn vault_word(vault: &Collections, class: &str, key: &str, name: &str) -> Result<u32, String> {
    let data = &vault.field(class, key, name)?.data;
    let hex: String = data.chars().filter(|c| !c.is_whitespace()).take(8).collect();
    u32::from_str_radix(&hex, 16).map_err(|e| format!("{class}/{key}/{name}: {e}"))
}

/// `Sk8::AudioSurfaceMap` (holder +64). `sub_82484198` indexes the array; materials outside
/// 0..94 read element 94 (`sub_82494E18`, `sub_82494EB8`, `sub_824DC2B0`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SurfaceMap {
    entries: Vec<[u32; 18]>,
}

impl SurfaceMap {
    pub(crate) fn load(vault: &Collections) -> Result<Self, String> {
        let field = vault.field(SURFACE_CLASS, SURFACE_KEY, SURFACE_FIELD)?;
        let array = field
            .array
            .as_ref()
            .ok_or("AudioSurfaceMap 4CA607558B1CF440 has no array payload")?;
        let entries = array
            .items
            .iter()
            .map(|item| {
                let hex: String = item.chars().filter(|c| !c.is_whitespace()).collect();
                if hex.len() != 144 {
                    return Err(format!("AudioSurfaceMap element of {} hex digits", hex.len()));
                }
                let mut words = [0; 18];
                for (i, word) in words.iter_mut().enumerate() {
                    *word = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).map_err(|e| e.to_string())?;
                }
                Ok(words)
            })
            .collect::<Result<Vec<_>, String>>()?;
        if entries.len() < 95 {
            return Err(format!("AudioSurfaceMap has {} elements, retail reads 95", entries.len()));
        }
        Ok(Self { entries })
    }

    /// The 32-bit word at byte `offset` of the material's element.
    pub(crate) fn lookup(&self, material: i32, offset: usize) -> u32 {
        let index = if (0..94).contains(&material) { material as usize } else { 94 };
        self.entries[index][offset / 4]
    }

    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<[u32; 18]>) -> Self {
        Self { entries }
    }
}

/// The audio-state fields the foot drag trigger and updater read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FootDragInputs {
    pub ground_speed_208: f32,
    pub hold_expired_310: bool,
    pub brake_336: bool,
    pub manual_brake_339: bool,
    /// +620 / +628: wheel 0 and wheel 2 materials (signed compares in `sub_824BA390`).
    pub wheel_material_620: i32,
    pub wheel_material_628: i32,
}

impl FootDragInputs {
    pub(crate) fn from_state(state: &AudioState) -> Option<Self> {
        Some(Self {
            ground_speed_208: state.ground_speed_208,
            hold_expired_310: state.hold_expired_310,
            brake_336: state.brake_336,
            manual_brake_339: state.manual_brake_339,
            wheel_material_620: state.wheel_material_620[0] as i32,
            wheel_material_628: state.wheel_material_620[2] as i32,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_capture(words: &[u32]) -> Self {
        use capture::{byte, float, int};
        Self {
            ground_speed_208: float(words, 208),
            hold_expired_310: byte(words, 310) != 0,
            brake_336: byte(words, 336) != 0,
            manual_brake_339: byte(words, 339) != 0,
            wheel_material_620: int(words, 620),
            wheel_material_628: int(words, 628),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FootDragTuning {
    /// Holder +24 `E5A6D8AC6EB9B5AB` (0.5): the trigger's speed offset.
    pub speed_offset: f32,
    /// Holder +24 `B2C81577820408BE` (50 km/h): the speed range.
    pub top_kmh: f32,
    /// Holder +24 `97171DE6035D6069`, `194EF41DA2254153`, `6242BC0480B09A33`,
    /// `9FF88541CAB461C7`: constructor words 9..12 (4000, 4500, 4000, 22500).
    pub levels: [i32; 4],
    /// Holder +140 `C58CE169C13320CA` (brake) / `2DBD9ED0AD824844` (manual brake): w14.
    pub eq_brake: i32,
    pub eq_manual: i32,
    pub surfaces: SurfaceMap,
}

impl FootDragTuning {
    pub(crate) fn load(vault: &Collections) -> Result<Self, String> {
        let int = |name: &str| vault.integer(TUNING_CLASS, DEFAULT_KEY, name).map(|v| v as i32);
        Ok(Self {
            speed_offset: vault.float(TUNING_CLASS, DEFAULT_KEY, "Hash_E5A6D8AC6EB9B5AB")?,
            top_kmh: vault.float(TUNING_CLASS, DEFAULT_KEY, "Hash_B2C81577820408BE")?,
            levels: [
                int("Hash_97171DE6035D6069")?,
                int("Hash_194EF41DA2254153")?,
                int("Hash_6242BC0480B09A33")?,
                int("Hash_9FF88541CAB461C7")?,
            ],
            eq_brake: vault_word(vault, EQ_CLASS, DEFAULT_KEY, "Hash_C58CE169C13320CA")? as i32,
            eq_manual: vault_word(vault, EQ_CLASS, DEFAULT_KEY, "Hash_2DBD9ED0AD824844")? as i32,
            surfaces: SurfaceMap::load(vault)?,
        })
    }
}

/// `sub_824BB540` / `sub_824BEEE8`: `+336 || +339 || (local && +310)`; `local` is owner byte
/// `[this+28]+72`.
pub(crate) fn foot_drag_active(inputs: &FootDragInputs, local: bool) -> bool {
    inputs.brake_336 || inputs.manual_brake_339 || (local && inputs.hold_expired_310)
}

/// The speed word both functions compute: `fctiwz(min(fsel(−x, 0, x), 1) × 10000)` with
/// x = (v − offset) / top × 3.6, or 500 on the local hold path.
fn drag_speed(inputs: &FootDragInputs, offset: f32, top_kmh: f32, local: bool) -> i32 {
    let x = (inputs.ground_speed_208 - offset) / top_kmh * KMH_PER_MS;
    let low = fsel(-x, 0.0, x);
    let unit = fsel(1.0 - low, low, 1.0);
    let speed = fctiwz(unit * TEN_THOUSAND);
    if local && inputs.hold_expired_310 { 500 } else { speed }
}

/// `sub_824BA390`: the foot surface from wheel 2's material while `+339`, else wheel 0's;
/// material ≥ 143 (none) → 0; AudioSurfaceMap `+20`; with `+339`, surface 1 → 0.
pub(crate) fn foot_surface(inputs: &FootDragInputs, surfaces: &SurfaceMap) -> i32 {
    let manual = inputs.manual_brake_339;
    let material = if manual { inputs.wheel_material_628 } else { inputs.wheel_material_620 };
    if material >= 143 {
        return 0;
    }
    let surface = surfaces.lookup(material, 20) as i32;
    if manual && surface == 1 { 0 } else { surface }
}

/// `sub_824BB540` → `sub_824AF498`: the posted packet.
pub(crate) fn foot_drag_constructor(tuning: &FootDragTuning, inputs: &FootDragInputs, local: bool) -> [u32; FOOT_DRAG_WORDS] {
    let speed = drag_speed(inputs, tuning.speed_offset, tuning.top_kmh, local);
    let eq = if inputs.manual_brake_339 { tuning.eq_manual } else { tuning.eq_brake };
    let mut words = [0; FOOT_DRAG_WORDS];
    words[1] = 32_767;
    words[5] = 25_000;
    words[7] = clamp_word(speed, 0, 10_000);
    words[8] = clamp_word(foot_surface(inputs, &tuning.surfaces), 0, 10);
    for (i, level) in tuning.levels.iter().enumerate() {
        words[9 + i] = clamp_word(*level, 0, 32_767);
    }
    words[13] = clamp_word(i32::from(!inputs.brake_336), 0, 2);
    words[14] = clamp_word(eq, 0, 32_767);
    words
}

/// `sub_824BEEE8`'s rewrite of a held packet (words 9..14 keep the constructor's values).
pub(crate) fn foot_drag_update(
    words: &mut [u32; FOOT_DRAG_WORDS],
    tuning: &FootDragTuning,
    inputs: &FootDragInputs,
    controls: &dyn Controls,
    local: bool,
) {
    let speed = drag_speed(inputs, UPDATER_SPEED_OFFSET, tuning.top_kmh, local);
    words[7] = clamp_word(speed, 0, 10_000);
    words[0] = 32_767;
    words[1] = clamp_word(controls.level(if inputs.brake_336 { 4 } else { 5 }) as i32, 0, 32_767);
    words[2] = clamp_word(controls.level(18) as i32, 0, 32_767);
    words[5] = clamp_word(controls.level(16) as i32, 0, 25_000);
    words[6] = clamp_word(controls.level(17) as i32, 0, 25_000);
    words[3] = clamp_word(controls.raw(0) as i32, 0, 0x1_0000);
    words[4] = clamp_word(controls.pitch(22), 0, 8_192);
    words[8] = clamp_word(foot_surface(inputs, &tuning.surfaces), 0, 10);
}

pub(crate) struct FootDrag {
    tuning: FootDragTuning,
    /// Owner byte +72: the engine drives only the local skater.
    local: bool,
    /// Component +128.
    held: Option<(u32, [u32; FOOT_DRAG_WORDS])>,
}

impl FootDrag {
    pub(crate) fn new(vault: &Collections) -> Result<Self, String> {
        Ok(Self {
            tuning: FootDragTuning::load(vault)?,
            local: true,
            held: None,
        })
    }
}

impl Component for FootDrag {
    fn process(&mut self, tick: &mut Tick) -> Result<(), String> {
        let Some(inputs) = FootDragInputs::from_state(tick.audio) else {
            return Ok(());
        };
        if foot_drag_active(&inputs, self.local) && self.held.is_none() {
            let words = foot_drag_constructor(&self.tuning, &inputs, self.local);
            let handle = post(tick.runtime, OBJECT, &words)?;
            self.held = Some((handle, words));
        }
        Ok(())
    }

    fn update(&mut self, tick: &mut Tick) -> Result<(), String> {
        let Some(inputs) = FootDragInputs::from_state(tick.audio) else {
            return Ok(());
        };
        if self.held.is_none() {
            return Ok(());
        }
        if !foot_drag_active(&inputs, self.local) {
            let mut handle = self.held.take().map(|(handle, _)| handle);
            return release(tick.runtime, &mut handle);
        }
        if let Some((handle, words)) = self.held.as_mut() {
            foot_drag_update(words, &self.tuning, &inputs, tick.controls, self.local);
            redeliver(tick.runtime, *handle, words)?;
        }
        Ok(())
    }
}

/// Retail recomp capture access for the tests (`.local/captures/extract`).
#[cfg(test)]
pub(crate) mod capture {
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;

    use super::Controls;

    pub(crate) fn root() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/captures/extract");
        root.join("state.tsv").exists().then_some(root)
    }

    fn slot(off: usize) -> usize {
        (off - 192) / 4
    }
    pub(crate) fn int(words: &[u32], off: usize) -> i32 {
        words[slot(off)] as i32
    }
    pub(crate) fn float(words: &[u32], off: usize) -> f32 {
        f32::from_bits(words[slot(off)])
    }
    pub(crate) fn byte(words: &[u32], off: usize) -> u8 {
        (words[slot(off)] >> (8 * (3 - (off - 192) % 4))) as u8
    }

    /// `state.tsv`: frame → the 160 words at state+192 (the last row of a frame wins).
    pub(crate) fn states(root: &PathBuf) -> BTreeMap<u32, Vec<u32>> {
        let text = std::fs::read_to_string(root.join("state.tsv")).unwrap();
        text.lines()
            .map(|line| {
                let mut parts = line.split('\t');
                let frame = parts.next().unwrap().parse().unwrap();
                parts.next();
                (frame, parts.map(|w| u32::from_str_radix(w, 16).unwrap()).collect())
            })
            .collect()
    }

    /// One `attributed/<object>.tsv` row.
    #[derive(Clone, Debug)]
    pub(crate) struct Row {
        pub kind: String,
        pub frame: u32,
        pub node: String,
        pub payload: String,
        pub words: Vec<u32>,
        pub ctrl: String,
        /// (fn, id, result, lr)
        pub reads: Vec<(u32, u32, u32, u32)>,
    }

    pub(crate) fn rows(root: &PathBuf, object: &str) -> Vec<Row> {
        let text = std::fs::read_to_string(root.join("attributed").join(format!("{object}.tsv"))).unwrap();
        text.lines()
            .map(|line| {
                let p: Vec<&str> = line.split('\t').collect();
                let hex = |s: &str| u32::from_str_radix(s, 16).unwrap();
                Row {
                    kind: p[0].into(),
                    frame: p[1].parse().unwrap(),
                    node: p[3].into(),
                    payload: p[4].into(),
                    words: if p[5] == "-" { vec![] } else { p[5].split(' ').map(hex).collect() },
                    ctrl: p[6].into(),
                    reads: p
                        .get(7)
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            s.split(',')
                                .map(|r| {
                                    let f: Vec<&str> = r.split(':').collect();
                                    (f[0].parse().unwrap(), f[1].parse().unwrap(), hex(f[2]), hex(f[3]))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The controller reads one retail function made (return address inside `lr_range`).
    #[derive(Default)]
    pub(crate) struct Captured(pub HashMap<(u32, u32), u32>);

    impl Captured {
        pub(crate) fn from_reads(reads: &[(u32, u32, u32, u32)], lr_range: std::ops::Range<u32>) -> Self {
            Self(
                reads
                    .iter()
                    .filter(|r| lr_range.contains(&r.3))
                    .map(|&(f, id, result, _)| ((if f == 64 { 60 } else { f }, id), result))
                    .collect(),
            )
        }
    }

    impl Controls for Captured {
        fn raw(&self, id: u32) -> u32 {
            self.0.get(&(52, id)).copied().unwrap_or(0)
        }
        fn pitch(&self, id: u32) -> i32 {
            self.0.get(&(56, id)).copied().unwrap_or(0) as i32
        }
        fn level(&self, id: u32) -> u32 {
            self.0.get(&(60, id)).copied().unwrap_or(0)
        }
    }

    /// Per-word exact-match counters.
    pub(crate) struct Matches {
        pub name: &'static str,
        pub total: usize,
        pub exact: Vec<usize>,
        pub bad: Vec<Vec<(u32, u32, u32)>>,
    }

    impl Matches {
        pub(crate) fn new(name: &'static str, words: usize) -> Self {
            Self { name, total: 0, exact: vec![0; words], bad: vec![vec![]; words] }
        }
        pub(crate) fn add(&mut self, frame: u32, retail: &[u32], ours: &[u32]) {
            self.total += 1;
            for (i, ours) in ours.iter().enumerate() {
                if retail[i] == *ours {
                    self.exact[i] += 1;
                } else if self.bad[i].len() < 3 {
                    self.bad[i].push((frame, retail[i], *ours));
                }
            }
        }
        pub(crate) fn print(&self) {
            println!("{}: {} rows", self.name, self.total);
            for (i, exact) in self.exact.iter().enumerate() {
                println!("  w{i:<2} {exact}/{}  bad (frame, retail, ours) {:?}", self.total, self.bad[i]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::capture::{Captured, Matches};
    use super::*;

    /// Vault values (`skater-collections.json`), and an AudioSurfaceMap whose +20 lane is the
    /// vault's for the materials used below (0 → 1, 2 → 0, 94 → 0).
    fn tuning() -> FootDragTuning {
        let mut entries = vec![[0u32; 18]; 95];
        entries[0][5] = 1;
        FootDragTuning {
            speed_offset: 0.5,
            top_kmh: 50.0,
            levels: [4000, 4500, 4000, 22500],
            eq_brake: 7,
            eq_manual: 7,
            surfaces: SurfaceMap::from_entries(entries),
        }
    }

    struct Fixed(&'static [(u32, u32, u32)]);
    impl Controls for Fixed {
        fn raw(&self, id: u32) -> u32 {
            self.0.iter().find(|r| r.0 == 52 && r.1 == id).map_or(0, |r| r.2)
        }
        fn pitch(&self, id: u32) -> i32 {
            self.0.iter().find(|r| r.0 == 56 && r.1 == id).map_or(0, |r| r.2 as i32)
        }
        fn level(&self, id: u32) -> u32 {
            self.0.iter().find(|r| r.0 == 60 && r.1 == id).map_or(0, |r| r.2)
        }
    }

    #[test]
    fn foot_drag_posts_on_brake_and_hold_with_the_vault_levels() {
        let tuning = tuning();
        let mut inputs = FootDragInputs { brake_336: true, ground_speed_208: 0.25, ..Default::default() };
        assert!(foot_drag_active(&inputs, true));
        // Retail post at frame 2746: w7 0 (below the 0.5 m/s offset), w13 = !336 = 0.
        assert_eq!(
            foot_drag_constructor(&tuning, &inputs, true),
            [0, 32767, 0, 0, 0, 25000, 0, 0, 1, 4000, 4500, 4000, 22500, 0, 7]
        );
        // Retail post at frame 6279: the local hold path, w7 = 500, w13 = 1.
        inputs = FootDragInputs { hold_expired_310: true, ground_speed_208: 9.0, wheel_material_620: 143, ..Default::default() };
        assert!(foot_drag_active(&inputs, true));
        assert!(!foot_drag_active(&inputs, false));
        assert_eq!(
            foot_drag_constructor(&tuning, &inputs, true),
            [0, 32767, 0, 0, 0, 25000, 0, 500, 0, 4000, 4500, 4000, 22500, 1, 7]
        );
    }

    #[test]
    fn foot_drag_update_reads_the_contacts_controller() {
        let tuning = tuning();
        let inputs = FootDragInputs { hold_expired_310: true, wheel_material_620: 143, ..Default::default() };
        let mut words = foot_drag_constructor(&tuning, &inputs, true);
        // Retail update at frame 6280 (controller reads captured with it).
        let controls = Fixed(&[(60, 5, 0x2055), (60, 18, 0x332), (60, 16, 0x618B), (60, 17, 0x4D), (52, 0, 0xFDF4), (56, 22, 0x11FD)]);
        foot_drag_update(&mut words, &tuning, &inputs, &controls, true);
        assert_eq!(
            words,
            [0x7FFF, 0x2055, 0x332, 0xFDF4, 0x11FD, 0x618B, 0x4D, 500, 0, 4000, 4500, 4000, 22500, 1, 7]
        );
    }

    #[test]
    fn foot_surface_uses_wheel_two_on_manual_brake_and_drops_surface_one() {
        let tuning = tuning();
        let mut inputs = FootDragInputs { wheel_material_620: 0, wheel_material_628: 0, ..Default::default() };
        assert_eq!(foot_surface(&inputs, &tuning.surfaces), 1);
        inputs.manual_brake_339 = true;
        assert_eq!(foot_surface(&inputs, &tuning.surfaces), 0);
        inputs.wheel_material_628 = 143;
        assert_eq!(foot_surface(&inputs, &tuning.surfaces), 0);
    }

    #[test]
    fn drag_speed_saturates_like_the_fsel_pair() {
        let tuning = tuning();
        let mut inputs = FootDragInputs { brake_336: true, ground_speed_208: 100.0, ..Default::default() };
        assert_eq!(foot_drag_constructor(&tuning, &inputs, true)[7], 10_000);
        inputs.ground_speed_208 = f32::NAN;
        // fsel(−NaN) takes NaN, then fsel(1 − NaN) takes 1.0.
        assert_eq!(foot_drag_constructor(&tuning, &inputs, true)[7], 10_000);
        inputs.ground_speed_208 = 5.0;
        // (5 − 0.5) / 50 × 3.6 = 0.324 → 3240.
        assert_eq!(foot_drag_constructor(&tuning, &inputs, true)[7], 3240);
    }

    fn vault() -> Option<Collections> {
        let root = std::path::PathBuf::from(r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets");
        root.join("private/stock/skater-collections.json")
            .exists()
            .then(|| Collections::load(&root).unwrap())
    }

    #[test]
    fn foot_drag_tuning_reads_the_vault() {
        let Some(vault) = vault() else { return };
        let tuning = FootDragTuning::load(&vault).unwrap();
        assert_eq!((tuning.speed_offset, tuning.top_kmh), (0.5, 50.0));
        assert_eq!(tuning.levels, [4000, 4500, 4000, 22500]);
        assert_eq!((tuning.eq_brake, tuning.eq_manual), (7, 7));
        // Element 94 (every material ≥ 94) has foot drag surface 0.
        assert_eq!(tuning.surfaces.lookup(200, 20), 0);
        // Element 0 (material 0) is 0, element 2 is 1 (stock `4CA607558B1CF440`).
        assert_eq!(tuning.surfaces.lookup(0, 20), 0);
        assert_eq!(tuning.surfaces.lookup(2, 20), 1);
    }

    /// Replays the retail capture: runs the component's trigger and updater over every captured
    /// frame (state from `lag` frames earlier) in retail's observed order (a post's first
    /// redelivery is on the next frame, so the frame's update runs before its process) and
    /// compares posts, releases and redelivered words.
    #[test]
    #[ignore = "needs .local/captures"]
    fn foot_drag_replays_the_retail_capture() {
        let Some(root) = super::capture::root() else { return };
        let Some(vault) = vault() else { return };
        let tuning = FootDragTuning::load(&vault).unwrap();
        let states = super::capture::states(&root);
        let rows = super::capture::rows(&root, OBJECT);
        const LOCAL: &str = "4A26A8B0";
        // The updater reads the previous frame's state row, the trigger the current one.
        for (update_lag, process_lag) in [(1u32, 0u32), (1, 1), (0, 0)] {
            let mut matches = Matches::new("foot drag updates", FOOT_DRAG_WORDS);
            let mut post_matches = Matches::new("foot drag posts", FOOT_DRAG_WORDS);
            let retail_posts: Vec<_> = rows.iter().filter(|r| r.kind == "PO").collect();
            let local_nodes: std::collections::HashSet<_> =
                rows.iter().filter(|r| r.kind == "UP" && r.ctrl == LOCAL).map(|r| r.node.clone()).collect();
            let retail_releases: Vec<u32> = rows
                .iter()
                .filter(|r| r.kind == "RL" && local_nodes.contains(&r.node))
                .map(|r| r.frame)
                .collect();
            let updates: std::collections::HashMap<u32, &super::capture::Row> =
                rows.iter().filter(|r| r.kind == "UP" && r.ctrl == LOCAL).map(|r| (r.frame, r)).collect();
            let mut held: Option<[u32; FOOT_DRAG_WORDS]> = None;
            let (mut our_posts, mut our_releases, mut our_updates) = (vec![], vec![], vec![]);
            for (&frame, _) in states.range(2709..) {
                let (Some(earlier), Some(state)) = (states.get(&(frame - update_lag)), states.get(&(frame - process_lag)))
                else {
                    continue;
                };
                let inputs = FootDragInputs::from_capture(earlier);
                // Update.
                if let Some(words) = held.as_mut() {
                    if !foot_drag_active(&inputs, true) {
                        held = None;
                        our_releases.push(frame);
                    } else {
                        our_updates.push(frame);
                        if let Some(row) = updates.get(&frame) {
                            let controls = Captured::from_reads(&row.reads, 0x824B_EEE8..0x824B_F268);
                            foot_drag_update(words, &tuning, &inputs, &controls, true);
                            matches.add(frame, &row.words, words);
                        }
                    }
                }
                // Process.
                let inputs = FootDragInputs::from_capture(state);
                if held.is_none() && foot_drag_active(&inputs, true) {
                    let words = foot_drag_constructor(&tuning, &inputs, true);
                    if let Some(row) = retail_posts.iter().find(|r| r.frame == frame) {
                        post_matches.add(frame, &row.words, &words);
                    }
                    held = Some(words);
                    our_posts.push(frame);
                }
            }
            println!("update lag {update_lag}, process lag {process_lag}");
            post_matches.print();
            matches.print();
            let retail_post_frames: Vec<u32> = retail_posts.iter().map(|r| r.frame).collect();
            let retail_update_frames: Vec<u32> = {
                let mut f: Vec<u32> = updates.keys().copied().collect();
                f.sort();
                f
            };
            println!("posts   retail {retail_post_frames:?}\n        ours   {our_posts:?}");
            println!("releases retail {retail_releases:?}\n         ours   {our_releases:?}");
            println!(
                "update frames: retail {} ours {} common {}",
                retail_update_frames.len(),
                our_updates.len(),
                our_updates.iter().filter(|f| updates.contains_key(f)).count()
            );
        }
    }
}
