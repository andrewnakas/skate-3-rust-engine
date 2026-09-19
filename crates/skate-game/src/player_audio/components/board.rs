//! `SFXObj_SkateBoard` (owner vtable `0x822FC770`): the board's continuous sound. The grain
//! rolling bed and its per-player bus chains, the per-surface and held `Class_rolling` layers,
//! the rattle, skid, squeaks and loose-board scrape, the owner's push envelopes and its MixMap
//! controller inputs.
//!
//! Transliterated from the lifted retail code (`skate3_recomp.11.cpp` unless noted):
//!
//! | retail | here |
//! |---|---|
//! | `sub_824C5058` constructor, `sub_824C59C8` start (players, held layers) | [`Board::new`] |
//! | `sub_824C6A78` process (vtable slot 9) | [`Component::process`] |
//! | `sub_824CA738` slope inputs 2/3 → `+1508`/`+1512` | [`Board::slope_inputs`] |
//! | `sub_824C5CA8` surface routing, `sub_824C82A8` / `sub_82494CD8` surface of a truck | [`Board::route_surfaces`], [`Board::surface_of`] |
//! | `sub_824C6198` push input 4, push envelopes, rattle post, `+1164`/`+1168` slews | [`Board::push_and_rattle`] |
//! | `sub_824C9058` chain pushes | [`Board::push_chains`] |
//! | `sub_824C7438` / `sub_824C72F0` skid trigger, input 1, `sub_824AF678` packet | [`Board::skid_trigger`], [`skid_predicate`], [`skid_post`] |
//! | `sub_824C7738` squeaks trigger, `sub_824AFF48` packet | [`Board::squeak_trigger`], [`squeak_post`] |
//! | `sub_824C9F68` spidercrack rolling layer 5 | [`Board::crack_layer`] |
//! | `sub_824CA448` / `sub_824CA318` seam-pattern gain envelope (`+1340`) | [`Board::seam_envelope`] |
//! | `sub_824CAEC0` graph-3 send level by speed | [`Board::distortion_send`] |
//! | `sub_824CB180` / `sub_824CB078` graph-1/3 gain wobble | [`Board::gain_wobble`] |
//! | `sub_824CB3C8` loose-board trigger, `sub_824B0670` packet | [`Board::board_slide_trigger`] |
//! | `sub_824C6BD8` update (vtable slot 10): grain records, per-surface rolling | [`Component::update`] |
//! | `sub_824C7A20` skid, `sub_824C7DD0` squeaks, `sub_824C80C0` rattle, `sub_824C9948` held layers, `sub_824CA038` layer 5, `sub_824CB4C0` board slide | the `*_update` functions |
//!
//! **Owner MixMap inputs** (controller vfunc 8, key `0x40010000`): id 0 surface-change pulse, 1
//! skid active, 2/3 downhill/uphill slope, 4 push foot planted, 6 on metal. They are buffered in
//! call order and taken with [`Board::take_owner_inputs`]; the retail MixMap evaluates them later
//! in the frame, so applying the buffer before the MixMap tick is equivalent.
//!
//! **Not ported** (reported): `sub_824CB828` (non-local skaters' pass-by), `sub_824CBAC0` (uses
//! `+1892/+1896`, the vfunc52(0) history `sub_824C6BD8` keeps, and `sub_824ADE30`), and
//! `sub_824C6BD8`'s store of vfunc52(0) × 360/65536 into the bus manager's `+124`/`+160`.
//!
//! **Surfaces.** Retail picks each truck's surface from the audio state's wheel material through
//! the vault's `Sk8::AudioSurfaceMap` (`0x4CA607558B1CF440`, entry `+4`). [`SurfacePolicy::Default`]
//! (the current default, accepted by the user) replaces every in-contact result with surface 2
//! (concrete_rough grains) until the engine's materials are trusted; [`SurfacePolicy::Retail`] is
//! the full mapping.

use std::collections::HashMap;
use std::sync::Arc;

use skate_audio_core::grain::board::{
    self as grain_board, BoardInputs, ChainInputs, GrainRecord, SlewInputs, SurfaceTuning,
};
use skate_audio_core::grain::chain::ChainConfig;
use skate_audio_core::grain::envelope::{Envelope, program_push};
use skate_data::collections::Collections;
use skate_data::audio::grains::GrainVault;

use super::super::audio_state::AudioState;
use super::words::{KMH_PER_MS, SPEED_SCALE, TEN_THOUSAND, THOUSAND, fctiwz, word};
use super::{Component, Controls, Tick, post, redeliver, release};

pub(crate) const ROLLING: &str = "Class_rolling";
pub(crate) const RATTLE: &str = "Rolling_Rattle_Class";
pub(crate) const SKID: &str = "Class_wheels_skid";
pub(crate) const SQUEAKS: &str = "Class_Squeaks";
pub(crate) const BOARD_SLIDE: &str = "c_board_slide";

/// hash64("default"), `sub_824C8370`'s key for surfaces without grains.
const DEFAULT_KEY: u64 = 0xD7ED_BD36_2D7D_2152;
/// `0x82256FE4`: the rolling max speed when no layer is given (`sub_824C6B30(−1)`).
const DEFAULT_ROLLING_KMH: f32 = 45.0;
/// `0x821747FC`.
const LEVEL: f32 = 32_767.0;
/// `lis -32241 ; lfs -10884`: 100.0; `lis -32243 ; lfs 29160`: 0.01; `0x822F8B3C`: −0.01.
const HUNDRED: f32 = 100.0;
const HUNDREDTH: f32 = f32::from_bits(0x3C23_D70A);
const MINUS_HUNDREDTH: f32 = f32::from_bits(0xBC23_D70A);
/// `0x822F9414`: 114.5916, radians to the squeak angle word.
const SQUEAK_ANGLE: f32 = f32::from_bits(0x42E5_2EE0);
/// `0x822F9018`: −90.0, the skid slip scale.
const MINUS_NINETY: f32 = -90.0;
/// `0x82256FD8`: 90.0.
const NINETY: f32 = 90.0;
/// `0x82165A00`: 0.05, the `+1168` step.
const BRAKE_STEP: f32 = f32::from_bits(0x3D4C_CCCD);
/// The title generator's state in the dumped image (`0x82FD7D74`).
const RNG_SEED: [u32; 6] = [
    0xF22D_0E56, 0x8831_26E9, 0xC624_DD2F, 0x0702_C49C, 0x9E35_3F7D, 0x6FDF_3B64,
];

/// How [`Board::surface_of`] turns a truck's contact into a surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfacePolicy {
    /// `sub_824C82A8` through the vault's AudioSurfaceMap.
    Retail,
    /// Retail's no-contact result (14) is kept; every other result becomes this surface. The
    /// accepted default is 2 (concrete_rough).
    Default(u32),
}

/// Owner facts retail reads from the character and the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoardConfig {
    /// `[[owner+16]+72]`: the local player (graph 3, both trucks, skid w14/w15/w16).
    pub local: bool,
    /// `[[owner+16]+64] == 0` (skid w14; 0 for the first player).
    pub first_player: bool,
    /// Skid w13: `[[0x830CFDC4]+564]` and its collection flag (0 in every capture post).
    pub global_flag: bool,
    pub surfaces: SurfacePolicy,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            local: true,
            first_player: true,
            global_flag: false,
            surfaces: SurfacePolicy::Default(2),
        }
    }
}

/// A seam pattern's gain wobble (`0x7242F32831ED3332`, `sub_824CA448`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wobble {
    gain_low: f32,
    gain_high: f32,
    ms_low: i32,
    ms_high: i32,
}

/// Every vault value the owner reads. Loaded once; a missing field is an error.
#[derive(Clone, Debug)]
pub(crate) struct BoardVault {
    /// `Sk8::AudioSurfaceMap` entries (18 words each), holder `+64`.
    surface_map: Vec<[u32; 18]>,
    /// `0x880C82E8EF647EC4`, rolling max speed per layer (holder `+56`).
    rolling_kmh: Vec<f32>,
    /// Grain-class collections by `sub_824C8370` key, plus `default`.
    tunings: HashMap<u64, SurfaceTuning>,
    /// Per key: rattle divisor `0x12275AA8AC4A63FB`, slope divisors `0x57A78D3BE8D47BB3` /
    /// `0x8DD4C3FC8DAF4059`.
    rattle_kmh: HashMap<u64, f32>,
    slope: HashMap<u64, (f32, f32)>,
    /// Holder `+4` (grain `default`): squeak threshold `0xA129B33B4A2C7961`, divisor
    /// `0xAC87D592E7601134`.
    squeak_threshold: i32,
    squeak_divisor: f32,
    /// Holder `+84`: skid w11 `0xFB10048CCDD6ADFA`, w12 `0x8CE42E5A9388A4C8`.
    skid_gain: i32,
    skid_level: i32,
    /// Holder `+140` eEQChain: rattle `0xC04832978CDED925`, skid `0xF52450E504250254`, squeak
    /// `0x22D0D4A5A14FFF7D`, board slide `0xF2B44F93BD91662E`.
    rattle_tweak: i32,
    skid_tweak: i32,
    squeak_tweak: i32,
    slide_tweak: i32,
    /// Holder `+96`: `0x9635B780C7472A6E`, `0x662CEE73D2E3FE2F`, `0xAB87C3D1EDDDDCBC`.
    slide_kmh: f32,
    slide_level: i32,
    slide_level_loose: i32,
    /// Holder `+132` (`sub_824CA938`): `+1520..+1552`, `+1564`, `+1568`, wobble records.
    chain: ChainConfig,
    send_ramp: [f32; 4],
    level_ramp: [f32; 3],
    wobble_ramp: [f32; 2],
    wobbles: [Wobble; 2],
    /// Seam-pattern wobbles by pattern 1..=15 (index 0 = `default`).
    seam: [Wobble; 16],
}

fn hash_name(key: u64) -> String {
    if key == DEFAULT_KEY {
        "default".to_owned()
    } else {
        format!("Hash_{key:016X}")
    }
}

impl BoardVault {
    pub(crate) fn load(assets: &std::path::Path) -> Result<Self, String> {
        let collections = Collections::load(assets)?;
        let grains = GrainVault::load(assets).map_err(|e| e.to_string())?;
        Self::from(&collections, &grains)
    }

    pub(crate) fn from(c: &Collections, grains: &GrainVault) -> Result<Self, String> {
        const HOLDER: &str = "Hash_C1831BDB6CB1B1EA";
        const GRAIN: &str = "Hash_7AB23C11B6ADA2DE";
        const EQ: &str = "Hash_42AFE160E647167C";
        const OWNER: &str = "Hash_6E878344774A7999";
        let map = c.field(HOLDER, "Hash_C489459A0C07D154", "Hash_4CA607558B1CF440")?;
        let items = &map
            .array
            .as_ref()
            .ok_or("AudioSurfaceMap has no array")?
            .items;
        let mut surface_map = Vec::new();
        for item in items {
            let mut entry = [0u32; 18];
            for (k, slot) in entry.iter_mut().enumerate() {
                *slot = u32::from_str_radix(item.get(k * 8..k * 8 + 8).ok_or("short map entry")?, 16)
                    .map_err(|e| e.to_string())?;
            }
            surface_map.push(entry);
        }
        let rolling_kmh = c.float_items(HOLDER, "Hash_7B0ED922C779B74C", "Hash_880C82E8EF647EC4")?;
        let int = |class: &str, key: &str, name: &str| -> Result<i32, String> {
            Ok(c.words::<1>(class, key, name)?[0] as i32)
        };
        let mut keys = vec![DEFAULT_KEY];
        for surface in 1..=9 {
            for soft in [false, true] {
                if let Some(choice) = grain_board::grain_for_surface(surface, soft) {
                    keys.push(choice.key);
                }
            }
        }
        let mut tunings = HashMap::new();
        let mut rattle_kmh = HashMap::new();
        let mut slope = HashMap::new();
        for key in keys {
            tunings.insert(key, grains.tuning(key).map_err(|e| e.to_string())?);
            let name = hash_name(key);
            rattle_kmh.insert(key, c.float(GRAIN, &name, "Hash_12275AA8AC4A63FB")?);
            slope.insert(
                key,
                (
                    c.float(GRAIN, &name, "Hash_57A78D3BE8D47BB3")?,
                    c.float(GRAIN, &name, "Hash_8DD4C3FC8DAF4059")?,
                ),
            );
        }
        let owner = |name: &str| c.float(OWNER, "default", name);
        let seam_names = [
            "spidercrack",
            "square_2_x_2",
            "square_4_x_4",
            "square_8_x_8",
            "square_12_x_12",
            "square_24_x_24",
            "irregular_small",
            "irregular_medium",
            "irregular_large",
            "slats",
            "sidewalk",
            "brick_tile_random_size",
            "mini_tile",
            "special_1",
            "special_2",
        ];
        const SEAM: &str = "Hash_7242F32831ED3332";
        let seam_of = |key: &str| -> Result<Wobble, String> {
            Ok(Wobble {
                gain_low: c.float(SEAM, key, "Hash_FA3A57801765A2F8")?,
                gain_high: c.float(SEAM, key, "Hash_32A9692F1B826274")?,
                ms_low: c.words::<1>(SEAM, key, "Hash_F713CB547B1DF920")?[0] as i32,
                ms_high: c.words::<1>(SEAM, key, "Hash_0608B3129FF81F12")?[0] as i32,
            })
        };
        let mut seam = [seam_of("default")?; 16];
        for (i, name) in seam_names.iter().enumerate() {
            let key = format!("Hash_{:016X}", skate_data::attrib_hash::hash(name));
            seam[i + 1] = seam_of(&key)?;
        }
        Ok(Self {
            surface_map,
            rolling_kmh,
            tunings,
            rattle_kmh,
            slope,
            squeak_threshold: int(GRAIN, "default", "Hash_A129B33B4A2C7961")?,
            squeak_divisor: c.float(GRAIN, "default", "Hash_AC87D592E7601134")?,
            skid_gain: int(HOLDER, "Hash_BA9837A6CF4C26ED", "Hash_FB10048CCDD6ADFA")?,
            skid_level: int(HOLDER, "Hash_BA9837A6CF4C26ED", "Hash_8CE42E5A9388A4C8")?,
            rattle_tweak: int(EQ, "default", "Hash_C04832978CDED925")?,
            skid_tweak: int(EQ, "default", "Hash_F52450E504250254")?,
            squeak_tweak: int(EQ, "default", "Hash_22D0D4A5A14FFF7D")?,
            slide_tweak: int(EQ, "default", "Hash_F2B44F93BD91662E")?,
            slide_kmh: c.float(HOLDER, "Hash_621090620F4F936A", "Hash_9635B780C7472A6E")?,
            slide_level: int(HOLDER, "Hash_621090620F4F936A", "Hash_662CEE73D2E3FE2F")?,
            slide_level_loose: int(HOLDER, "Hash_621090620F4F936A", "Hash_AB87C3D1EDDDDCBC")?,
            chain: ChainConfig {
                local: true,
                create: 0,
                eq_chain: int(EQ, "default", "Hash_A5D3ADA63608617F")? as u32,
                clip: owner("Hash_E64C04ED542DABC8")?,
                shelf_corner: owner("Hash_55BEB30353F244A9")?,
                shelf_gain: owner("Hash_45516395725ED16B")?,
                local_send: 0.0,
            },
            send_ramp: [
                owner("Hash_0D665393E2EDC605")?,
                owner("Hash_28E708782445747F")?,
                owner("Hash_D900C07BE7C5450F")?,
                0.0,
            ],
            level_ramp: [
                owner("Hash_88AA96B08FD16914")?,
                owner("Hash_3FFB5107C82BA3E0")?,
                owner("Hash_D3E8894CA25A4F71")?,
            ],
            wobble_ramp: [owner("Hash_281A501B22B6CCDF")?, owner("Hash_54CDE019E31FC04E")?],
            wobbles: [
                Wobble {
                    ms_low: int(OWNER, "default", "Hash_36AE41817640FE04")?,
                    ms_high: int(OWNER, "default", "Hash_71EE27313BD30F21")?,
                    gain_low: owner("Hash_437D128B53669C34")?,
                    gain_high: owner("Hash_02885338DD5D7DCA")?,
                },
                Wobble {
                    ms_low: int(OWNER, "default", "Hash_2055BBF39C152FA9")?,
                    ms_high: int(OWNER, "default", "Hash_F5240AFADA3B3FFC")?,
                    gain_low: owner("Hash_F916E153393C5F24")?,
                    gain_high: owner("Hash_0A36F90732016D85")?,
                },
            ],
            seam,
        })
    }

    fn tuning(&self, key: u64) -> &SurfaceTuning {
        self.tunings
            .get(&key)
            .unwrap_or_else(|| &self.tunings[&DEFAULT_KEY])
    }

    /// `sub_824C97B8(layer)`; −1 is the image's 45 km/h.
    fn rolling_kmh(&self, layer: i32) -> f32 {
        if layer < 0 {
            return DEFAULT_ROLLING_KMH;
        }
        self.rolling_kmh.get(layer as usize).copied().unwrap_or(0.0)
    }

    /// `sub_82494CD8` / `sub_824C7388`: material → map entry (≥ 94 uses entry 94).
    fn map_entry(&self, material: u32) -> &[u32; 18] {
        let index = if (material as i32) < 94 && (material as i32) >= 0 {
            material as usize
        } else {
            94
        };
        &self.surface_map[index.min(self.surface_map.len() - 1)]
    }
}

/// The title generator (`sub_82A8AF10`), a private instance seeded with the dumped image's state.
/// Retail shares one generator with about a hundred call sites, so its sequence cannot be
/// reproduced; the recurrence is the retail one.
#[derive(Clone, Debug)]
pub(crate) struct TitleRng([u32; 6]);

impl TitleRng {
    fn carry(sum: u32, old: u32, carry_in: u32) -> u32 {
        u32::from(sum < old || (sum == old && carry_in != 0))
    }

    pub(crate) fn next(&mut self) -> u32 {
        let s = &mut self.0;
        let s5 = s[5];
        let r10 = s[4].wrapping_add(s5);
        let c1 = u32::from(r10 < s5);
        let r30 = s[3].wrapping_add(r10).wrapping_add(c1);
        let c2 = Self::carry(r30, r10, c1);
        let r31 = s[2].wrapping_add(r30).wrapping_add(c2);
        let c3 = Self::carry(r31, r30, c2);
        let r5 = s[1].wrapping_add(r31).wrapping_add(c3);
        let c4 = Self::carry(r5, r31, c3);
        let r3 = s[0].wrapping_add(r5).wrapping_add(c4);
        *s = [r3, r5, r31, r30, r10, s5.wrapping_add(1)];
        if s[5] == 0 {
            for k in [4usize, 3, 2, 1, 0] {
                s[k] = s[k].wrapping_add(1);
                if s[k] != 0 {
                    break;
                }
            }
        }
        s[0]
    }
}

// ------------------------------------------------------------------ packet words (pure)

fn clamp(value: i32, high: i32) -> u32 {
    word(value, 0, high)
}

/// `fctiwz(clamp01(x) × scale)` with the updaters' two `fsel`s.
fn unit_word(ratio: f32, scale: f32) -> i32 {
    let clamped = if !(-ratio >= 0.0) { ratio } else { 0.0 };
    let clamped = if 1.0 - clamped >= 0.0 { clamped } else { 1.0 };
    fctiwz(clamped * scale)
}

/// `sub_824C6B30` (and `sub_824C9830`'s form): `fctiwz(clamp01(v / kmh × 3.6) × 10000)`.
pub(crate) fn speed_word(speed: f32, kmh: f32) -> i32 {
    unit_word(speed / kmh * KMH_PER_MS, TEN_THOUSAND)
}

/// `sub_824C4C18`: a `Class_rolling` post.
pub(crate) fn rolling_post(speed: i32, selector: i32, surface: i32) -> [u32; 12] {
    [
        0,
        0,
        4096,
        clamp(speed, 10_000),
        clamp(selector, 15),
        0,
        clamp(surface, 13),
        0,
        0,
        25_000,
        0,
        32_767,
    ]
}

/// `sub_824C9948` / `sub_824CA038` for one held layer; `surface` is `None` when both trucks are
/// off the ground and not grinding (the word keeps its value).
pub(crate) fn held_rolling_update(
    words: &mut [u32; 12],
    c: &dyn Controls,
    gain_id: u32,
    speed: i32,
    surface: Option<i32>,
    audio: &AudioState,
) {
    words[0] = 32_767;
    words[11] = clamp(c.level(gain_id) as i32, 32_767);
    words[1] = clamp(c.raw(0) as i32, 65_536);
    words[2] = clamp(c.pitch(8), 8192);
    words[3] = clamp(speed, 10_000);
    words[5] = 0;
    if let Some(surface) = surface {
        words[6] = clamp(surface, 13);
    }
    words[7] = u32::from(audio.balance_340 || audio.manual_brake_339);
    words[8] = clamp(c.level(19) as i32, 32_767);
    words[9] = clamp(c.level(17) as i32, 25_000);
    words[10] = clamp(c.level(18) as i32, 25_000);
}

/// `sub_824C6BD8`'s per-surface `Class_rolling` rewrite.
pub(crate) fn surface_rolling_update(
    words: &mut [u32; 12],
    c: &dyn Controls,
    speed: i32,
    selector: i32,
    audio: &AudioState,
) {
    words[0] = 32_767;
    words[11] = clamp(c.level(1) as i32, 32_767);
    words[1] = clamp(c.raw(0) as i32, 65_536);
    words[2] = clamp(c.pitch(3), 8192);
    words[3] = clamp(speed, 10_000);
    words[5] = 0;
    words[8] = if selector == 1 {
        0
    } else {
        clamp(c.level(13) as i32, 32_767)
    };
    words[9] = clamp(c.level(11) as i32, 25_000);
    words[10] = clamp(c.level(12) as i32, 25_000);
    words[7] = u32::from(audio.balance_340 || audio.manual_brake_339);
}

/// `sub_824B0248`: a rattle post.
pub(crate) fn rattle_post(speed: i32, code: i32, tweak: i32) -> [u32; 12] {
    [
        0,
        0,
        4096,
        clamp(speed, 10_000),
        clamp(code, 8),
        0,
        1,
        0,
        25_000,
        0,
        32_767,
        clamp(tweak, 32_767),
    ]
}

/// `sub_824C80C0`.
pub(crate) fn rattle_update(words: &mut [u32; 12], c: &dyn Controls) {
    let pitch = c.pitch(3);
    words[0] = 32_767;
    words[10] = clamp(c.level(6) as i32, 32_767);
    words[1] = clamp(c.raw(0) as i32, 65_535);
    words[2] = clamp(pitch, 8192);
    words[7] = clamp(c.level(16) as i32, 32_767);
    words[8] = clamp(c.level(14) as i32, 25_000);
    words[9] = clamp(c.level(15) as i32, 25_000);
}

/// `sub_824C72F0`. `revert` is `+690`; on the path where the lifted code has overwritten its
/// state pointer with the trick id, retail reads the byte at guest address `trick + 690` instead
/// (low memory, 0 in the recomp), so that path only tests the counter.
pub(crate) fn skid_predicate(audio: &AudioState, counter: i32) -> bool {
    if audio.slip_232 > 0.0 {
        // The grinding test (`+341 ? +192 == 4`) is computed and then overwritten: dead code.
        if !(audio.walking_716 && audio.board_held_308) {
            let trick = audio.audio_trick_348 as i32;
            if trick == -1 || trick == 35 {
                return true;
            }
            return counter > 0;
        }
    }
    audio.revert_690 || counter > 0
}

/// `sub_824C7388`: the skid surface, map entry `+12` of wheel 0's material (0 with none).
fn skid_surface(vault: &BoardVault, audio: &AudioState) -> i32 {
    let material = audio.wheel_material_620[0];
    if material as i32 >= 143 {
        0
    } else {
        vault.map_entry(material)[3] as i32
    }
}

/// `sub_824AF678`: the skid post (18 words).
#[allow(clippy::too_many_arguments)]
pub(crate) fn skid_post(
    speed: i32,
    soft: i32,
    surface: i32,
    slip: i32,
    gain: i32,
    level: i32,
    global: i32,
    first_local: i32,
    local: i32,
    local_level: i32,
    tweak: i32,
) -> [u32; 18] {
    [
        0,
        32_767,
        0,
        0,
        0,
        25_000,
        0,
        clamp(speed, 10_000),
        clamp(soft, 1),
        clamp(surface, 4),
        clamp(slip, 90),
        clamp(gain, 32_767),
        clamp(level, 32_767),
        clamp(global, 1),
        clamp(first_local, 1),
        clamp(local, 1),
        clamp(local_level, 32_767),
        clamp(tweak, 32_767),
    ]
}

/// `fctiwz(clamp01(v × 0.08) × scale)`, the skid (10000) and squeak (1000) speed words.
fn slow_speed(speed: f32, scale: f32) -> i32 {
    unit_word(speed * SPEED_SCALE, scale)
}

/// `sub_824C7A20`'s words (the counter is updated by the caller first).
pub(crate) fn skid_update(
    words: &mut [u32; 18],
    c: &dyn Controls,
    audio: &AudioState,
    counter: i32,
    surface: i32,
    local: bool,
) {
    words[7] = clamp(slow_speed(audio.ground_speed_208, TEN_THOUSAND), 10_000);
    words[4] = clamp(c.pitch(3), 8192);
    words[0] = 32_767;
    words[1] = clamp(c.level(4) as i32, 32_767);
    words[3] = clamp(c.raw(0) as i32, 65_536);
    words[2] = clamp(c.level(13) as i32, 32_767);
    words[5] = clamp(c.level(11) as i32, 25_000);
    words[6] = clamp(c.level(12) as i32, 25_000);
    let slip = fctiwz(audio.slip_232 * MINUS_NINETY);
    words[10] = clamp(counter.wrapping_sub(slip).min(90), 90);
    words[9] = clamp(surface, 4);
    words[16] = clamp(if local { c.level(20) as i32 } else { 0 }, 32_767);
}

/// The squeak angle word `|z| / 1.5 × 1000`, 1000 at most, 0 below 50.
fn squeak_turn(z: f32, divisor: f32) -> i32 {
    let value = fctiwz(z.abs() / divisor * THOUSAND);
    if value > 1_000 {
        1_000
    } else if value < 50 {
        0
    } else {
        value
    }
}

/// `sub_824AFF48`: the squeak post (11 words).
pub(crate) fn squeak_post(speed: i32, turn: i32, tweak: i32) -> [u32; 11] {
    [
        0,
        32_767,
        0,
        0,
        4096,
        25_000,
        0,
        clamp(speed, 1_000),
        15,
        clamp(turn, 1_000),
        clamp(tweak, 32_767),
    ]
}

/// `sub_824C7DD0`.
pub(crate) fn squeak_update(words: &mut [u32; 11], c: &dyn Controls, audio: &AudioState, divisor: f32) {
    let speed = slow_speed(audio.ground_speed_208, THOUSAND);
    let turn = squeak_turn(audio.deck_angular_velocity_480[2], divisor);
    words[7] = clamp(speed, 1_000);
    words[9] = clamp(turn, 1_000);
    words[4] = clamp(c.pitch(3), 8192);
    words[0] = 32_767;
    words[1] = clamp(c.level(5) as i32, 32_767);
    words[2] = 0;
    words[5] = clamp(c.level(11) as i32, 25_000);
    words[6] = clamp(c.level(12) as i32, 25_000);
    words[3] = clamp(c.raw(0) as i32, 65_536);
}

/// `sub_824B0670`: the loose-board scrape post (12 words).
pub(crate) fn board_slide_post(tweak: i32, loose: i32) -> [u32; 12] {
    [0, 0, 4096, 0, 25_000, 0, 0, 0, clamp(tweak, 32_767), 0, clamp(loose, 3), 0]
}

/// `sub_824CB4C0`.
pub(crate) fn board_slide_update(
    words: &mut [u32; 12],
    c: &dyn Controls,
    audio: &AudioState,
    vault: &BoardVault,
) {
    words[0] = 32_767;
    words[1] = clamp(c.raw(0) as i32, 65_535);
    words[2] = clamp(c.pitch(23), 8192);
    let ratio = (audio.ground_speed_208 - 0.5) / vault.slide_kmh * KMH_PER_MS;
    words[3] = clamp(unit_word(ratio, TEN_THOUSAND), 10_000);
    words[4] = clamp(c.level(25) as i32, 25_000);
    words[5] = clamp(c.level(26) as i32, 25_000);
    words[6] = clamp(c.level(27) as i32, 32_767);
    words[7] = clamp(c.level(24) as i32, 32_767);
    let level = if audio.loose_board_780 == 2 {
        vault.slide_level_loose
    } else {
        vault.slide_level
    };
    words[11] = clamp(level, 32_767);
}

// ------------------------------------------------------------------ the owner

/// One truck's side of the owner (`owner + 4t`, `+16t`, `+48t` fields).
#[derive(Clone, Debug)]
struct Truck {
    /// `+768`: current surface (14 = none).
    surface: u32,
    /// `+184+16t`: the grain-class collection key this truck's attribute instance holds.
    key: Option<u64>,
    /// `+1312`: the per-surface `Class_rolling` handle and words.
    rolling: Option<(u32, [u32; 12])>,
    /// `+1488`: its selector.
    selector: u32,
    /// `+1320`: the truck's surface plays grains.
    grain_surface: bool,
    /// `+1328`: grains running.
    grains: bool,
    /// `+1496`: the truck has a live sound for its surface.
    live: bool,
    /// Players A and B (`+1176`, `+1180`).
    players: [u32; 2],
}

pub(crate) struct Board {
    config: BoardConfig,
    vault: BoardVault,
    trucks: [Truck; 2],
    /// `+1500`.
    primary: usize,
    /// `+760`: the last `sub_824C8370` key.
    key_760: u64,
    /// `+1288`, `+1292`/`+1296`, `+1300`, `+1304`, `+1308`, `+1332`, `+1884`.
    skid: Option<(u32, [u32; 18])>,
    squeak: Option<(u32, [u32; 11])>,
    squeak_sign: bool,
    rattle: Option<(u32, [u32; 12])>,
    layer0: Option<(u32, [u32; 12])>,
    layer3: Option<(u32, [u32; 12])>,
    layer5: Option<(u32, [u32; 12])>,
    board_slide: Option<(u32, [u32; 12])>,
    /// `+1516`.
    skid_counter: i32,
    latch_1504: bool,
    latch_1505: bool,
    /// `+912`, `+1036`, `+1340`.
    scale_envelope: Envelope,
    shift_envelope: Envelope,
    seam_envelope: Envelope,
    /// `+1336`, `+1464`, `+1468`, `+1472..+1484`.
    seam_pattern: u32,
    seam_active: bool,
    seam_target: f32,
    seam_wobble: Wobble,
    f1160: f32,
    f1164: f32,
    f1168: f32,
    f1508: f32,
    f1512: f32,
    f1556: f32,
    f1560: f32,
    /// The two `+1572`/`+1728` wobble records: envelope, target, last posted, gains.
    wobble_envelopes: [Envelope; 2],
    wobble_targets: [f32; 2],
    wobble_last: [f32; 2],
    rng: TitleRng,
    owner_inputs: Vec<(u32, u32)>,
}

impl Board {
    /// `sub_824C5058` + `sub_824C59C8`: load the grain members, create the four players, and (local)
    /// post the held `Class_rolling` layers 0 and 3 (`sub_824C9830`).
    pub(crate) fn new(
        tick: &mut Tick,
        vault: BoardVault,
        members: &[(String, Vec<u8>, Arc<[i16]>, u8, u32)],
        config: BoardConfig,
    ) -> Result<Self, String> {
        let mut players = [[0u32; 2]; 2];
        {
            let mut grains = tick.runtime.grains();
            for (name, bytes, samples, channels, rate) in members {
                grains
                    .load(name, bytes, samples.clone(), *channels, *rate)
                    .map_err(|e| e.to_string())?;
            }
            for truck in &mut players {
                for player in truck.iter_mut() {
                    *player = grains.create_player().map_err(|e| e.to_string())?;
                }
            }
        }
        let truck = |players: [u32; 2], key| Truck {
            surface: 14,
            key,
            rolling: None,
            selector: 0,
            grain_surface: true,
            grains: false,
            live: false,
            players,
        };
        let mut board = Self {
            config,
            // The constructor's last `sub_824C5BF8` leaves truck 0's instance on wood_ramp_soft.
            trucks: [
                truck(players[0], Some(0x382B_1263_6ED9_D8DA)),
                truck(players[1], None),
            ],
            vault,
            primary: 0,
            key_760: DEFAULT_KEY,
            skid: None,
            squeak: None,
            squeak_sign: false,
            rattle: None,
            layer0: None,
            layer3: None,
            layer5: None,
            board_slide: None,
            skid_counter: 0,
            latch_1504: false,
            latch_1505: false,
            scale_envelope: Envelope::default(),
            shift_envelope: Envelope::default(),
            seam_envelope: Envelope::default(),
            seam_pattern: 0,
            seam_active: false,
            seam_target: 0.0,
            seam_wobble: Wobble {
                gain_low: 1.0,
                gain_high: 1.0,
                ms_low: 0,
                ms_high: 0,
            },
            f1160: 0.0,
            f1164: 0.0,
            f1168: 0.0,
            f1508: 0.0,
            f1512: 0.0,
            f1556: 0.0,
            f1560: 1.0,
            wobble_envelopes: [Envelope::default(), Envelope::default()],
            wobble_targets: [0.0; 2],
            wobble_last: [0.0; 2],
            rng: TitleRng(RNG_SEED),
            owner_inputs: Vec::new(),
        };
        // sub_824C5058 fills the wobble records' +16 with 0 and +20..+28 with 1.0 (sub_824C59C8).
        board.wobble_last = [1.0; 2];
        if board.config.local {
            board.post_held_layers(tick)?;
        }
        Ok(board)
    }

    /// The owner MixMap inputs written since the last call, in retail order.
    pub(crate) fn take_owner_inputs(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.owner_inputs)
    }

    fn set_input(&mut self, id: u32, value: u32) {
        self.owner_inputs.push((id, value));
    }

    fn tuning_of(&self, truck: usize) -> &SurfaceTuning {
        self.vault.tuning(self.trucks[truck].key.unwrap_or(DEFAULT_KEY))
    }

    /// `sub_824C9830`.
    fn post_held_layers(&mut self, tick: &mut Tick) -> Result<(), String> {
        let speed = tick.audio.ground_speed_208;
        let zero = speed_word(speed, self.vault.rolling_kmh(0));
        let words = rolling_post(zero, 0, 3);
        self.layer0 = Some((post(tick.runtime, ROLLING, &words)?, words));
        let three = speed_word(speed, self.vault.rolling_kmh(3));
        let words = rolling_post(three, 3, 3);
        self.layer3 = Some((post(tick.runtime, ROLLING, &words)?, words));
        Ok(())
    }

    /// `sub_824CA688`, updating the `+1504` latch.
    fn manual_latch(&mut self, audio: &AudioState) -> bool {
        self.latch_1504 = grain_board::manual_latch(self.latch_1504, audio.balance_340, audio.wheel_count_200);
        self.latch_1504
    }

    /// `sub_824CA6E0`, updating the `+1505` latch.
    fn trick_latch(&mut self, audio: &AudioState) -> bool {
        self.latch_1505 = grain_board::trick_latch(
            self.latch_1505,
            audio.hippy_jump_372,
            audio.foot_in_deck_box_615,
            audio.foot_in_deck_box_616,
        );
        self.latch_1505
    }

    fn special(&mut self, audio: &AudioState) -> bool {
        self.manual_latch(audio) || self.trick_latch(audio)
    }

    /// `sub_824C82A8`: truck `t`'s surface, 14 when it has none.
    pub(crate) fn surface_of(&mut self, audio: &AudioState, truck: usize) -> u32 {
        if self.config.local && self.manual_latch(audio) {
            let landed = if truck == self.primary {
                audio.wheel_landed_464[0]
            } else {
                audio.wheel_landed_464[3]
            };
            if !landed {
                return 14;
            }
        }
        if audio.grinding_341 {
            return 14;
        }
        let material = if truck == self.primary {
            audio.wheel_material_620[0]
        } else {
            audio.wheel_material_620[3]
        };
        let retail = if material as i32 >= 143 {
            3
        } else {
            self.vault.map_entry(material)[1]
        };
        match self.config.surfaces {
            SurfacePolicy::Retail => retail,
            SurfacePolicy::Default(surface) => surface,
        }
    }

    /// `sub_824C6B30(layer)`: the speed word, scaled by the push envelope while it runs.
    fn scaled_speed_word(&self, audio: &AudioState, layer: i32) -> i32 {
        let mut speed = audio.ground_speed_208;
        if !self.scale_envelope.idle {
            speed *= self.scale_envelope.value;
        }
        speed_word(speed, self.vault.rolling_kmh(layer))
    }

    /// `sub_824CA738`: `+1508`/`+1512` from the state's slope `+712` and inputs 2 and 3.
    pub(crate) fn slope_inputs(&mut self, slope_712: f32) {
        self.f1508 = 0.0;
        self.f1512 = 0.0;
        let primary = self.primary;
        if self.trucks[primary].grain_surface {
            let key = self.trucks[primary].key.unwrap_or(DEFAULT_KEY);
            let (down, up) = self.vault.slope.get(&key).copied().unwrap_or((0.0, 0.0));
            if slope_712 < 0.0 {
                self.f1508 = unit_ratio(slope_712 / down);
            } else if slope_712 > 0.0 {
                self.f1512 = unit_ratio(slope_712 / up);
            }
        }
        let a = clamp(fctiwz(self.f1508 * LEVEL), 32_767);
        self.set_input(2, a);
        let b = clamp(fctiwz(self.f1512 * LEVEL), 32_767);
        self.set_input(3, b);
    }

    fn stop_truck_sound(&mut self, tick: &mut Tick, t: usize) -> Result<(), String> {
        if !self.trucks[t].grain_surface {
            let mut holder = self.trucks[t].rolling.take().map(|(h, _)| h);
            release(tick.runtime, &mut holder)?;
        } else if self.trucks[t].grains {
            let mut grains = tick.runtime.grains();
            for &player in &self.trucks[t].players {
                grains.stop(player).map_err(|e| e.to_string())?;
            }
            // sub_824C4D50 (chain teardown) is not run: chains are kept (grain::chain note).
            self.trucks[t].grains = false;
        }
        self.trucks[t].live = false;
        Ok(())
    }

    /// `sub_824C5CA8`.
    pub(crate) fn route_surfaces(&mut self, tick: &mut Tick) -> Result<(), String> {
        self.set_input(0, 0);
        for iteration in 0..2 {
            let mut t = self.primary;
            if iteration == 1 {
                t = usize::from(self.primary == 0);
            }
            let mut other = usize::from(t == 0);
            let surface = self.surface_of(tick.audio, t);
            let current = self.trucks[t].surface;
            if surface != current {
                let mut release_old = true;
                if current != 14 {
                    self.set_input(0, 32_767);
                    if !self.config.local && !self.trucks[other].live {
                        std::mem::swap(&mut t, &mut other);
                        self.primary = usize::from(self.primary == 0);
                        release_old = false;
                    }
                    if release_old {
                        self.stop_truck_sound(tick, t)?;
                    }
                }
                self.trucks[t].surface = surface;
                if surface != 14 {
                    self.start_truck_sound(tick, t, other, surface)?;
                }
            }
            if !self.config.local {
                break;
            }
        }
        let first = self.surface_of(tick.audio, 0);
        self.set_input(6, if first == 9 { 32_767 } else { 0 });
        Ok(())
    }

    fn start_truck_sound(
        &mut self,
        tick: &mut Tick,
        t: usize,
        other: usize,
        surface: u32,
    ) -> Result<(), String> {
        let soft = tick.audio.soft_wheels_684 != 0;
        let choice = grain_board::grain_for_surface(surface, soft);
        let key = choice.map_or(DEFAULT_KEY, |c| c.key);
        self.key_760 = key;
        self.trucks[t].key = Some(key);
        let grain_surface = !matches!(surface, 7 | 8 | 10..=13);
        if surface != self.trucks[other].surface {
            if !grain_surface {
                let selector = match surface {
                    7 => 1,
                    8 => 2,
                    10 => 10,
                    11 => 12,
                    12 => 11,
                    13 => 9,
                    _ => -1,
                };
                self.trucks[t].selector = selector as u32;
                let words = rolling_post(0, selector, surface as i32);
                self.trucks[t].rolling = Some((post(tick.runtime, ROLLING, &words)?, words));
                self.trucks[t].live = true;
            } else if let Some(choice) = choice {
                let tuning = self.vault.tuning(key).clone();
                let chain = ChainConfig {
                    local: self.config.local,
                    ..self.vault.chain
                };
                let mut grains = tick.runtime.grains();
                for which in 0..2 {
                    let player = self.trucks[t].players[which];
                    let bus = grains.chain(player, &chain).map_err(|e| e.to_string())?;
                    grains
                        .bind(player, choice.member, tuning.params[which], bus)
                        .map_err(|e| e.to_string())?;
                }
                self.trucks[t].grains = true;
                self.trucks[t].live = true;
            }
        }
        self.trucks[t].grain_surface = grain_surface;
        Ok(())
    }

    /// `sub_824C6198`.
    pub(crate) fn push_and_rattle(&mut self, tick: &mut Tick) -> Result<(), String> {
        let audio = tick.audio;
        let planted = audio.push_left_333 || audio.push_right_334;
        self.set_input(4, if planted { 32_767 } else { 0 });
        if audio.push_plant_335 {
            let primary_key = self.trucks[self.primary].key.unwrap_or(DEFAULT_KEY);
            let push = self.vault.tuning(primary_key).push;
            program_push(&push, audio.ground_speed_208, &mut self.scale_envelope, &mut self.shift_envelope);
            let mut holder = self.rattle.take().map(|(h, _)| h);
            release(tick.runtime, &mut holder)?;
            if self.trucks[self.primary].grain_surface {
                let divisor = self.vault.rattle_kmh.get(&primary_key).copied().unwrap_or(30.0);
                let over = (audio.ground_speed_208 - 1.0) * KMH_PER_MS;
                let speed = unit_word(over / divisor, TEN_THOUSAND);
                let code = match self.key_760 {
                    0x7C59_12FC_2DAB_F98C => 1,
                    0x0372_1D0F_A99A_03C8 => 2,
                    0xFFB5_E3E6_2E0B_4943 => 3,
                    0x7947_A259_F181_FDB4 => 4,
                    0xB303_AED8_2415_30E2 => 5,
                    _ => 0,
                };
                let words = rattle_post(speed, code, self.vault.rattle_tweak);
                self.rattle = Some((post(tick.runtime, RATTLE, &words)?, words));
            }
        }
        if !self.scale_envelope.idle {
            self.scale_envelope.advance(tick.dt)?;
        }
        if !self.shift_envelope.idle {
            self.shift_envelope.advance(tick.dt)?;
        }
        // sub_824C8588.
        let latch = self.manual_latch(audio);
        let ca6e0 = if latch { false } else { self.trick_latch(audio) };
        let v = audio.com_velocity_96;
        let tuning = self.tuning_of(self.primary).clone();
        let slew = grain_board::slew(
            &tuning,
            &SlewInputs {
                vector: [v[0], v[1], v[2], 0.0],
                factor: audio.turn_204,
                state_200: audio.wheel_count_200 as i32,
                state_340: audio.balance_340,
                latch_1504: latch,
                ca6e0,
                previous: self.f1160,
            },
        );
        self.f1160 = slew.f1160;
        self.f1164 = slew.f1164;
        // The +1168 brake slew.
        let toward_one = audio.brake_336 && {
            let d = audio.com_velocity_delta_128;
            let d = grain_board::normalised_dot([d[0], d[1], d[2], 0.0], [v[0], v[1], v[2], 0.0]);
            d < 0.0
        };
        let f0 = self.f1168;
        self.f1168 = if toward_one {
            if 1.0 - f0 > BRAKE_STEP {
                f0 + BRAKE_STEP
            } else if f0 - 1.0 > BRAKE_STEP {
                f0 - BRAKE_STEP
            } else {
                1.0
            }
        } else if -f0 > BRAKE_STEP {
            f0 + BRAKE_STEP
        } else if f0 > BRAKE_STEP {
            f0 - BRAKE_STEP
        } else {
            0.0
        };
        Ok(())
    }

    /// `sub_824C9058`.
    pub(crate) fn push_chains(&mut self, tick: &mut Tick) -> Result<(), String> {
        let c = tick.controls;
        for t in 0..2 {
            if !self.trucks[t].grains || !self.trucks[t].grain_surface {
                continue;
            }
            let special = self.special(tick.audio);
            let tuning = self.tuning_of(t).clone();
            let primary = self.tuning_of(self.primary).clone();
            let values = grain_board::chain_values(
                &tuning,
                &ChainInputs {
                    mix64_11: c.level(11) as i32,
                    mix64_12: c.level(12) as i32,
                    mix52_0: c.raw(0) as i32,
                    mix60_13: c.level(13) as i32,
                    local: self.config.local.then(|| (c.level(21) as i32, c.level(22) as i32)),
                    special,
                    f1152: (!self.shift_envelope.idle).then_some(self.shift_envelope.value),
                    boost: self.f1508,
                    boost_shift_a: primary.shift_boost,
                    boost_shift_b: primary.shift_boost_b,
                },
            );
            let [a, b] = self.trucks[t].players;
            tick.runtime
                .grains()
                .push_chain(a, b, &values)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// `sub_824C7438`.
    pub(crate) fn skid_trigger(&mut self, tick: &mut Tick) -> Result<(), String> {
        let audio = tick.audio;
        if !skid_predicate(audio, self.skid_counter) {
            self.set_input(1, 0);
            return Ok(());
        }
        self.set_input(1, 32_767);
        if self.skid.is_some() {
            return Ok(());
        }
        let local_level = if self.config.local {
            tick.controls.level(20) as i32
        } else {
            0
        };
        let words = skid_post(
            slow_speed(audio.ground_speed_208, TEN_THOUSAND),
            i32::from(audio.soft_wheels_684 != 0),
            skid_surface(&self.vault, audio),
            fctiwz(audio.slip_232 * NINETY),
            self.vault.skid_gain,
            self.vault.skid_level,
            i32::from(self.config.global_flag),
            i32::from(self.config.local && self.config.first_player),
            i32::from(self.config.local),
            local_level,
            self.vault.skid_tweak,
        );
        self.skid = Some((post(tick.runtime, SKID, &words)?, words));
        Ok(())
    }

    /// `sub_824C7738`.
    pub(crate) fn squeak_trigger(&mut self, tick: &mut Tick) -> Result<(), String> {
        let audio = tick.audio;
        if audio.foot_in_deck_box_615 && audio.foot_in_deck_box_616 && audio.wheel_count_200 as i32 > 1 {
            let angle = fctiwz(audio.deck_tilt_264.abs() * SQUEAK_ANGLE);
            if angle < self.vault.squeak_threshold {
                return Ok(());
            }
            let sign = audio.deck_tilt_264 >= 0.0;
            if self.squeak_sign != sign && self.squeak.is_some() {
                let mut holder = self.squeak.take().map(|(h, _)| h);
                release(tick.runtime, &mut holder)?;
            }
            self.squeak_sign = sign;
            if self.squeak.is_none() {
                let words = squeak_post(
                    slow_speed(audio.ground_speed_208, THOUSAND),
                    squeak_turn(audio.deck_angular_velocity_480[2], self.vault.squeak_divisor),
                    self.vault.squeak_tweak,
                );
                self.squeak = Some((post(tick.runtime, SQUEAKS, &words)?, words));
            }
        } else {
            let mut holder = self.squeak.take().map(|(h, _)| h);
            release(tick.runtime, &mut holder)?;
        }
        Ok(())
    }

    /// `sub_824C9F68`: rolling layer 5 while wheel 0 is on the spidercrack pattern.
    pub(crate) fn crack_layer(&mut self, tick: &mut Tick) -> Result<(), String> {
        let audio = tick.audio;
        let mut pattern = audio.wheel_seam_636[0];
        if self.manual_latch(audio) && !audio.wheel_landed_464[0] {
            pattern = audio.wheel_seam_636[3];
        }
        let speed = self.scaled_speed_word(audio, 5);
        match (&self.layer5, pattern == 1) {
            (None, true) => {
                let words = rolling_post(speed, 5, 3);
                self.layer5 = Some((post(tick.runtime, ROLLING, &words)?, words));
            }
            (Some(_), false) => {
                let mut holder = self.layer5.take().map(|(h, _)| h);
                release(tick.runtime, &mut holder)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `sub_824CA318`: a random gain in `[+1472, +1476]` and duration in `[+1480, +1484]`.
    fn seam_random(&mut self) -> (f32, i32) {
        let w = self.seam_wobble;
        let mut gain = w.gain_low;
        if w.gain_low != w.gain_high {
            let range = fctiwz((w.gain_high - w.gain_low) * HUNDRED);
            let draw = self.rng.next();
            gain = if range > 0 {
                let m = draw % range as u32;
                m as i32 as f32 * HUNDREDTH
            } else {
                let range = range.unsigned_abs().max(1);
                let m = draw % range;
                m as i32 as f32 * MINUS_HUNDREDTH
            };
            gain += w.gain_low;
        }
        let mut ms = w.ms_low;
        if w.ms_high != w.ms_low {
            let range = w.ms_high.wrapping_sub(w.ms_low).unsigned_abs().max(1);
            ms = (self.rng.next() % range) as i32 + w.ms_low;
        }
        (gain, ms)
    }

    /// `sub_824CA448` (local only).
    pub(crate) fn seam_envelope(&mut self, tick: &mut Tick) -> Result<(), String> {
        if !self.config.local {
            return Ok(());
        }
        let pattern = tick.audio.wheel_seam_636[0];
        if pattern != self.seam_pattern {
            self.seam_envelope.reset();
            self.seam_pattern = pattern;
            self.seam_active = false;
            if pattern != 0 {
                let index = if (1..=15).contains(&pattern) { pattern as usize } else { 0 };
                self.seam_wobble = self.vault.seam[index];
                if self.seam_wobble.gain_low != 1.0 || self.seam_wobble.gain_high != 1.0 {
                    self.seam_active = true;
                    let (gain, ms) = self.seam_random();
                    self.seam_envelope.reset();
                    self.seam_envelope.add(1.0, gain, ms);
                    self.seam_target = gain;
                }
            }
        } else if !self.seam_envelope.idle {
            self.seam_envelope.advance(tick.dt)?;
        }
        Ok(())
    }

    /// `sub_824CAEC0` (local only): the graph-1 → graph-3 send level and `+1560` from speed.
    pub(crate) fn distortion_send(&mut self, tick: &mut Tick) -> Result<(), String> {
        if !self.config.local {
            return Ok(());
        }
        let kmh = tick.audio.ground_speed_208 * KMH_PER_MS;
        let [low, high, level, _] = self.vault.send_ramp;
        let mut send = 0.0;
        if kmh >= high {
            send = level;
        } else if kmh >= low {
            send = (kmh - low) / (high - low) * level;
        }
        let [start, end, floor] = self.vault.level_ramp;
        if kmh >= end {
            self.f1560 = floor;
        } else if kmh >= start {
            let frac = (kmh - start) / (end - start);
            self.f1560 = 1.0 - frac * (1.0 - floor);
        }
        if send != self.f1556 {
            self.f1556 = send;
            let records: Vec<u32> = self
                .trucks
                .iter()
                .flat_map(|t| t.players)
                .filter_map(|p| tick.runtime.grains().chain_record(p))
                .collect();
            let mut grains = tick.runtime.grains();
            for record in records {
                let module = grains.g.u32(grains.g.u32(record).map_err(|e| e.to_string())? + 16).map_err(|e| e.to_string())?;
                skate_audio_core::device::post_property(grains.g, module, 0, f64::from(send))
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// `sub_824CB078` + `sub_824CB180` (local only): the graph-1/3 `Gai0` gain wobble.
    pub(crate) fn gain_wobble(&mut self, tick: &mut Tick) -> Result<(), String> {
        if !self.config.local {
            return Ok(());
        }
        for k in 0..2 {
            if self.wobble_envelopes[k].idle {
                let w = self.vault.wobbles[k];
                let span = (w.ms_high - w.ms_low).max(1) as u32;
                let ms = (self.rng.next() % span) as i32 + w.ms_low;
                let range = fctiwz((w.gain_high - w.gain_low) * HUNDRED).max(1) as u32;
                let m = self.rng.next() % range;
                let mut target = (m as i32 as f32).mul_add(HUNDREDTH, w.gain_low);
                if !(self.wobble_targets[k] < 0.0) {
                    target *= -1.0;
                }
                self.wobble_envelopes[k].reset();
                self.wobble_envelopes[k].add(self.wobble_targets[k], target, ms);
                self.wobble_targets[k] = target;
            } else {
                self.wobble_envelopes[k].advance(tick.dt)?;
            }
        }
        let kmh = tick.audio.ground_speed_208 * KMH_PER_MS;
        let [low, high] = self.vault.wobble_ramp;
        let ramp = if kmh >= high {
            1.0
        } else if kmh >= low {
            (kmh - low) / (high - low)
        } else {
            0.0
        };
        let gains: [f32; 2] =
            std::array::from_fn(|k| self.wobble_envelopes[k].value.mul_add(ramp, 1.0));
        for (index, t) in [0usize, 1].into_iter().enumerate() {
            let _ = index;
            for which in 0..2 {
                let current = gains[which];
                if current == self.wobble_last[which] {
                    continue;
                }
                self.wobble_last[which] = current;
                let player = self.trucks[t].players[which];
                let Some(record) = tick.runtime.grains().chain_record(player) else {
                    continue;
                };
                let mut grains = tick.runtime.grains();
                let g = |at: u32| -> Result<u32, String> { Ok(at) };
                let _ = g;
                let mods1 = grains.g.u32(record).map_err(|e| e.to_string())?;
                let mods3 = grains.g.u32(record + 16).map_err(|e| e.to_string())?;
                let gain1 = grains.g.u32(mods1 + 20).map_err(|e| e.to_string())?;
                skate_audio_core::device::post_property(
                    grains.g,
                    gain1,
                    0,
                    f64::from(current * self.f1560),
                )
                .map_err(|e| e.to_string())?;
                if mods3 != 0 {
                    let gain3 = grains.g.u32(mods3 + 8).map_err(|e| e.to_string())?;
                    skate_audio_core::device::post_property(grains.g, gain3, 0, f64::from(current))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// `sub_824CB3C8`.
    pub(crate) fn board_slide_trigger(&mut self, tick: &mut Tick) -> Result<(), String> {
        let loose = tick.audio.loose_board_780;
        match (&self.board_slide, loose) {
            (None, 0) | (Some(_), 1..) => {}
            (None, _) => {
                let words = board_slide_post(self.vault.slide_tweak, i32::from(loose == 2));
                self.board_slide = Some((post(tick.runtime, BOARD_SLIDE, &words)?, words));
            }
            (Some(_), 0) => {
                let mut holder = self.board_slide.take().map(|(h, _)| h);
                release(tick.runtime, &mut holder)?;
            }
        }
        Ok(())
    }

    /// `sub_824C7A20`.
    fn skid_updater(&mut self, tick: &mut Tick) -> Result<(), String> {
        if self.skid.is_none() {
            return Ok(());
        }
        let audio = tick.audio;
        if !skid_predicate(audio, self.skid_counter) {
            let mut holder = self.skid.take().map(|(h, _)| h);
            release(tick.runtime, &mut holder)?;
        }
        if audio.revert_690 {
            self.skid_counter = (self.skid_counter + 5).min(45);
        } else if self.skid_counter != 0 {
            if self.skid_counter > 0 {
                self.skid_counter -= 15;
            }
            self.skid_counter = self.skid_counter.max(0);
        }
        let surface = skid_surface(&self.vault, audio);
        let counter = self.skid_counter;
        let local = self.config.local;
        if let Some((handle, words)) = self.skid.as_mut() {
            skid_update(words, tick.controls, audio, counter, surface, local);
            redeliver(tick.runtime, *handle, words)?;
        }
        Ok(())
    }

    /// The `w6` surface the held layers use: the primary truck's, else the other's; `None` when
    /// both are 14 and not grinding (grinding writes 13).
    fn layer_surface(&mut self, audio: &AudioState) -> Option<i32> {
        let mut surface = self.surface_of(audio, self.primary);
        if surface == 14 {
            let other = usize::from(self.primary == 0);
            surface = self.surface_of(audio, other);
            if surface == 14 {
                return audio.grinding_341.then_some(13);
            }
        }
        Some(surface as i32)
    }
}

fn unit_ratio(ratio: f32) -> f32 {
    let clamped = if !(-ratio >= 0.0) { ratio } else { 0.0 };
    if 1.0 - clamped >= 0.0 { clamped } else { 1.0 }
}

impl Component for Board {
    /// `sub_824C6A78`.
    fn process(&mut self, tick: &mut Tick) -> Result<(), String> {
        // `+712` is not in the audio state yet (reported); the slope inputs see level ground.
        self.slope_inputs(0.0);
        self.route_surfaces(tick)?;
        self.push_and_rattle(tick)?;
        self.push_chains(tick)?;
        self.skid_trigger(tick)?;
        self.squeak_trigger(tick)?;
        self.crack_layer(tick)?;
        self.seam_envelope(tick)?;
        self.distortion_send(tick)?;
        self.gain_wobble(tick)?;
        self.board_slide_trigger(tick)?;
        Ok(())
    }

    /// `sub_824C6BD8`.
    fn update(&mut self, tick: &mut Tick) -> Result<(), String> {
        let audio = tick.audio;
        let c = tick.controls;
        for t in 0..2 {
            if !self.trucks[t].live {
                continue;
            }
            if !self.trucks[t].grain_surface {
                let selector = self.trucks[t].selector as i32;
                let speed = self.scaled_speed_word(audio, selector);
                if let Some((handle, words)) = self.trucks[t].rolling.as_mut() {
                    surface_rolling_update(words, c, speed, selector, audio);
                    redeliver(tick.runtime, *handle, words)?;
                }
                continue;
            }
            if !self.trucks[t].grains {
                continue;
            }
            let special = self.special(audio);
            let tuning = self.tuning_of(t).clone();
            let primary = self.tuning_of(self.primary).clone();
            let input = BoardInputs {
                speed: audio.ground_speed_208,
                speed_scale: (!self.scale_envelope.idle).then_some(self.scale_envelope.value),
                mix_gain_a: c.level(1) as i32,
                mix_gain_b: c.level(2) as i32,
                mix_pitch: c.pitch(3),
                f1164: self.f1164,
                f1168: self.f1168,
                f1456: self.seam_active.then_some(self.seam_envelope.value),
                special,
                boost: self.f1508,
                boost_gain: primary.boost_gain,
                boost_kmh: primary.boost_kmh,
            };
            let [a, b]: [GrainRecord; 2] = grain_board::board_records(&tuning, &input);
            let [pa, pb] = self.trucks[t].players;
            let mut grains = tick.runtime.grains();
            grains.set_record(pa, a).map_err(|e| e.to_string())?;
            grains.set_record(pb, b).map_err(|e| e.to_string())?;
        }
        self.skid_updater(tick)?;
        if let Some((handle, words)) = self.squeak.as_mut() {
            squeak_update(words, c, audio, self.vault.squeak_divisor);
            redeliver(tick.runtime, *handle, words)?;
        }
        if let Some((handle, words)) = self.rattle.as_mut() {
            rattle_update(words, c);
            redeliver(tick.runtime, *handle, words)?;
        }
        let surface = self.layer_surface(audio);
        let speed0 = speed_word(audio.ground_speed_208, self.vault.rolling_kmh(0));
        let speed3 = speed_word(audio.ground_speed_208, self.vault.rolling_kmh(3));
        for (layer, gain_id, speed) in [(0usize, 7u32, speed0), (3, 9, speed3)] {
            let held = if layer == 0 { &mut self.layer0 } else { &mut self.layer3 };
            if let Some((handle, words)) = held.as_mut() {
                held_rolling_update(words, c, gain_id, speed, surface, audio);
                redeliver(tick.runtime, *handle, words)?;
            }
        }
        let speed5 = self.scaled_speed_word(audio, 5);
        if let Some((handle, words)) = self.layer5.as_mut() {
            held_rolling_update(words, c, 10, speed5, surface, audio);
            redeliver(tick.runtime, *handle, words)?;
        }
        // The seam envelope's next segment (sub_824C6BD8's tail).
        if self.seam_active && self.seam_envelope.idle {
            let (gain, ms) = self.seam_random();
            self.seam_envelope.reset();
            if self.seam_target < 1.0 {
                self.seam_envelope.add(self.seam_target, 1.0, ms);
                self.seam_target = 1.0;
            } else {
                self.seam_envelope.add(1.0, gain, ms);
                self.seam_target = gain;
            }
        }
        if let Some((handle, words)) = self.board_slide.as_mut() {
            board_slide_update(words, c, audio, &self.vault);
            redeliver(tick.runtime, *handle, words)?;
        }
        Ok(())
    }
}
