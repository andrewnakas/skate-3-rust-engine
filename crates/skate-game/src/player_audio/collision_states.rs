//! `CSTATEMGR_Collision` — the contact-sound subsystem `sub_82486EF0` posts to, and the sink the
//! grind onset and the continuous per-material impact level both need (`docs/engine-defects.md`
//! #9).
//!
//! Retail's audio is a three-level, data-driven object model (`docs/audio-object-registry.md`):
//! a `CSTATEMGR_<X>` manager owns N `CSTATE_<X>` slots, and each slot owns one `SFXObj_<Y>`
//! component. For collision, `CSTATEMGR_Collision::vtable[+8]` (`sub_824F17B8`) builds **exactly
//! ten** `CSTATE_Collision` slots on `[manager+16]`, each with one `SFXObj_Collision`.
//!
//! The delivery `sub_82486EF0` performs is two hops:
//!
//! 1. `[manager+668]->vtable[+12]` = `sub_824F1818`, a *router*: it walks the ten slots and keeps
//!    the one with the lowest signed `[node+64]`, calls that node's `vtable[+28]`
//!    (`sub_824F8B58`, which frees the message the slot was still holding) and returns it.
//! 2. that node's `vtable[+12]` = `sub_824F8990`, five instructions: stamp `[node+64]` from a
//!    global counter, park the message at `[node+68]`, activate the state.
//!
//! `[node+64]` being a *timestamp* is what makes the router an LRU: the slot that has gone
//! longest without a message is the one reused. [`CollisionStates`] is that manager.
//!
//! Two things come out of the component per frame:
//!
//! * `sub_824D1E00` (`SFXObj_Collision::vtable[+36]`) rewrites two controller inputs from zero
//!   every frame — a gate and a weight picked from the message's two tier words. That weight is
//!   retail's *continuous* landing dynamic, the part §9 of `player-audio-retail-drivers.md`
//!   identified as missing. See [`CollisionStates::drive`] and [`contact_weight`].
//! * `sub_824D1F68` starts up to two voices, one per material in the message, and `sub_824D2318`
//!   keeps their properties up to date. Which sample a material plays is read out of the
//!   controller's own outputs — `sub_824D20E8` and `sub_824D22B8` pick *which output id* from the
//!   material's category. See [`CollisionMaterials`].
//!
//! **What is not here yet.** `sub_824965D0` → `sub_824967F8` turn (material, other material,
//! tier) into the actual sample id, and that pair is a further undecoded layer. Everything in
//! this module is the control plane around it, so the seam is explicit: [`Slot::started`] records
//! the conditions retail checks *before* it calls the chooser, and the chooser's own `-1` is not
//! modelled. Nothing here invents a sample.

use skate_data::collections::Collections;

use super::collision_materials::{MATERIAL_COUNT, MATERIAL_SOUND_ID, MATERIAL_VAULT_KEY};

/// `sub_824F17B8` loops exactly ten times.
pub(crate) const SLOT_COUNT: usize = 10;

/// The contact code's "no material" sentinel — the value the table at `0x8302D6E8` stops at, and
/// the one `sub_824D1F68` skips a voice record on.
pub(crate) const NO_MATERIAL: i32 = MATERIAL_COUNT as i32;

/// The message's "no tier" sentinel. `sub_824D1F68` skips a voice record whose tier word is 3, and
/// `sub_824D1E00` treats 3 as "take the other one" when it picks the weight.
pub(crate) const NO_TIER: i32 = 3;

/// `sub_82496FD0` returns 8 for a negative material.
const NEGATIVE_MATERIAL_CATEGORY: u8 = 8;

/// The category `sub_824D20E8` forces for `material >= 143`. Note `sub_824D22B8` does *not* use
/// this one — it takes the `category == 9` branch instead, so the two must stay separate.
const HIGH_MATERIAL_LEVEL_CATEGORY: u8 = 8;

/// Where a material with no vault record lands. `sub_82496FD0` pre-seeds its lookup with the
/// default record at `0x830D0850` and reads `+72` back whether or not the lookup replaced it; that
/// record is all zeroes in the image, so an unresolvable material reads category 0.
const DEFAULT_CATEGORY: u8 = 0;

/// `sub_824D20E8`'s jump table at `0x824D212C`, category `0..=9` → the controller output id it
/// reads the material's level from.
const CATEGORY_LEVEL_OUTPUT: [u32; 10] = [13, 14, 15, 16, 17, 18, 12, 19, 20, 21];

/// `sub_824D20E8`'s `cmplwi r3,9; bgt` arm: any category past the table falls here.
const LEVEL_OUTPUT_FALLBACK: u32 = 20;

/// `sub_824D22B8`: category 9 (or `material >= 143`) reads output 22, everything else output 1.
/// Both are the MixMap's `t1` (signed, `× 4096`) outputs — see the `Controls::pitch` accessor
/// `sub_824C5910`, which is the same function the component calls here.
const PITCH_OUTPUT_CATEGORY_9: u32 = 22;
const PITCH_OUTPUT_OTHER: u32 = 1;

/// `sub_824D1E00`'s two controller inputs: id 0 is the gate, id 1 the weight.
const GATE_INPUT: u32 = 0;
const WEIGHT_INPUT: u32 = 1;

/// The gate's raised value, and the ceiling the weight is clamped to.
const FULL_SCALE: u32 = 32_767;

/// The vault class of a material record, and the field `sub_82496FD0` reads its category from.
/// The field's retail type is `Sk8::Audio::eMaterialNicotineType`; it takes exactly ten values
/// across the 145 records, which is what the `0..=9` jump table above indexes.
const MATERIAL_CLASS: &str = "Hash_D40CB4C0FFE45676";
const MATERIAL_CATEGORY_FIELD: &str = "Hash_D5EF686287A57AFE";

/// The controller key of one collision slot. `sub_828DEA00` packs a component's key as
/// `0x40000000 | category << 16 | index << 11 | kind << 4`; collision is category 3, kind 0, so
/// the ten slots are `40030000`, `40030800`, … `40034800` — which is exactly what the retail
/// MixMap holds (`cargo run -p skate-audio-core --example mixmap_dump`, ten `SFXObj_Collision`
/// controllers whose outputs run 0..=22 with 1 and 22 typed `t1`).
pub(crate) fn controller_key(slot: usize) -> u32 {
    0x4003_0000 | ((slot as u32) << 11)
}

/// `sub_82486EF0`'s 48-byte message, field for field.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ContactMessage {
    /// `+0x00`: the first material — for a grind onset the family base (95 or 96).
    pub material_a: i32,
    /// `+0x04`: the second material — for a grind onset the grind material (143 → 10).
    pub material_b: i32,
    /// `+0x08`: the first material's impact tier, 3 for "none".
    pub tier_a: i32,
    /// `+0x0C`: the second material's impact tier, 3 for "none".
    pub tier_b: i32,
    /// `+0x10`: a `vec4` world position, copied wholesale from the audio state (`+48` for a grind
    /// onset, `+144` for a landing).
    pub position: [f32; 4],
    /// `+0x20` / `+0x24`: the two `0..=32767` levels from `sub_82496C58`'s interpolation.
    pub level_a: u32,
    pub level_b: u32,
    /// `+0x28` / `+0x29` / `+0x2A`.
    pub flags: [u8; 3],
}

impl ContactMessage {
    /// The material of voice record `i`, as `sub_824D1F68` indexes them (`msg+0`, `msg+4`).
    pub(crate) fn material(&self, record: usize) -> i32 {
        if record == 0 { self.material_a } else { self.material_b }
    }

    /// The tier of voice record `i` (`msg+8`, `msg+12`).
    pub(crate) fn tier(&self, record: usize) -> i32 {
        if record == 0 { self.tier_a } else { self.tier_b }
    }
}

/// `sub_824D1E00`'s weight: the message's two tier words choose one of three fixed levels.
///
/// 3 is a "none" sentinel — if one word is 3 the other is taken, and only when neither is 3 does
/// the larger win. The clamp is retail's own and is vacuous for these three constants, but it is
/// what the lifted code does so it is kept.
pub(crate) fn contact_weight(tier_a: i32, tier_b: i32) -> u32 {
    let selected = if tier_a == NO_TIER {
        tier_b
    } else if tier_b == NO_TIER {
        tier_a
    } else {
        tier_a.max(tier_b)
    };
    let level: i32 = match selected {
        1 => 20_000,
        2 => 32_767,
        _ => 10_000,
    };
    level.clamp(0, FULL_SCALE as i32) as u32
}

/// The per-material collision table: the baked image data plus the categories read from the
/// owner's vault.
pub(crate) struct CollisionMaterials {
    category: [u8; MATERIAL_COUNT],
}

impl CollisionMaterials {
    /// Resolve every material's category through the vault, the way `sub_82496FD0` does one at a
    /// time. A material whose record is absent from this installation falls back the way retail's
    /// pre-seeded default record does; the count of those is returned so a caller can report it.
    pub(crate) fn load(vault: &Collections) -> (Self, usize) {
        let mut category = [DEFAULT_CATEGORY; MATERIAL_COUNT];
        let mut unresolved = 0;
        for (material, slot) in category.iter_mut().enumerate() {
            let key = format!("Hash_{:016X}", MATERIAL_VAULT_KEY[material]);
            match vault.field(MATERIAL_CLASS, &key, MATERIAL_CATEGORY_FIELD) {
                Ok(field) => match u32::from_str_radix(field.data.trim(), 16) {
                    Ok(value) => *slot = value as u8,
                    Err(_) => unresolved += 1,
                },
                Err(_) => unresolved += 1,
            }
        }
        (Self { category }, unresolved)
    }

    /// For tests and for callers that already know the table.
    #[cfg(test)]
    pub(crate) fn from_categories(category: [u8; MATERIAL_COUNT]) -> Self {
        Self { category }
    }

    /// `sub_82496FD0`.
    pub(crate) fn category(&self, material: i32) -> u8 {
        if material < 0 {
            return NEGATIVE_MATERIAL_CATEGORY;
        }
        self.category
            .get(material as usize)
            .copied()
            .unwrap_or(DEFAULT_CATEGORY)
    }

    /// The collision sound id at `+0` of the material's entry. `-1` means the material is silent,
    /// and `sub_824965D0` returns "no sound" without consulting the chooser at all.
    pub(crate) fn sound_id(&self, material: i32) -> i32 {
        if !(0..MATERIAL_COUNT as i32).contains(&material) {
            return -1;
        }
        MATERIAL_SOUND_ID[material as usize]
    }

    /// `sub_824D20E8`: which controller output carries this material's level.
    pub(crate) fn level_output(&self, material: i32) -> u32 {
        let category = if material >= NO_MATERIAL {
            HIGH_MATERIAL_LEVEL_CATEGORY
        } else {
            self.category(material)
        };
        CATEGORY_LEVEL_OUTPUT
            .get(category as usize)
            .copied()
            .unwrap_or(LEVEL_OUTPUT_FALLBACK)
    }

    /// `sub_824D22B8`: which controller output carries this material's pitch. `material >= 143`
    /// takes the category-9 branch here, *not* the category-8 one `level_output` forces.
    pub(crate) fn pitch_output(&self, material: i32) -> u32 {
        if material >= NO_MATERIAL || self.category(material) == 9 {
            PITCH_OUTPUT_CATEGORY_9
        } else {
            PITCH_OUTPUT_OTHER
        }
    }
}

/// One `CSTATE_Collision` slot and the `SFXObj_Collision` hanging off it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Slot {
    /// `[node+64]`: the stamp the router compares. Retail takes it from a global counter, so the
    /// lowest is the least recently used.
    pub stamp: i32,
    /// `[node+68]` (and `SFXObj_Collision+36`): the parked message.
    pub message: Option<ContactMessage>,
    /// `[state+52]`: raised by `sub_828DF6F0` on activation, cleared when both voice records go
    /// idle (`sub_824D2318`'s tail calls the state's `vtable[+28]`).
    pub active: bool,
    /// `SFXObj_Collision+40` / `+72`: whether each of the two voice records holds a voice. These
    /// are set from the conditions `sub_824D1F68` checks before it calls the sample chooser; the
    /// chooser's own refusal is the undecoded part (see the module note).
    pub started: [bool; 2],
}

/// The ten-slot LRU manager.
#[derive(Clone, Debug, Default)]
pub(crate) struct CollisionStates {
    slots: [Slot; SLOT_COUNT],
    /// The global counter `sub_824F8990` stamps from (`[[0x830CFD94]+16]`).
    clock: i32,
}

impl CollisionStates {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn slots(&self) -> &[Slot; SLOT_COUNT] {
        &self.slots
    }

    /// `sub_82486EF0`'s two hops: route to the least recently used slot, evict what it was
    /// holding, then stamp, park and activate. Returns the slot the message landed on.
    pub(crate) fn post(&mut self, message: ContactMessage, materials: &CollisionMaterials) -> usize {
        // `sub_824F1818` walks from the head and keeps a node only when it is *strictly* lower,
        // so the first slot at the minimum wins a tie — which is what `min_by_key` does too.
        let slot = (0..SLOT_COUNT)
            .min_by_key(|&index| self.slots[index].stamp)
            .expect("SLOT_COUNT is not zero");
        // `vtable[+28]` (`sub_824F8B58`) frees the message the slot still held before reuse.
        let state = &mut self.slots[slot];
        state.message = None;
        state.started = [false; 2];
        // `sub_824F8990`, then `sub_828DF6F0`.
        self.clock = self.clock.wrapping_add(1);
        let state = &mut self.slots[slot];
        state.stamp = self.clock;
        state.message = Some(message);
        state.active = true;
        // `sub_824D1F68`: one voice record per material, each skipped on the two sentinels.
        for record in 0..2 {
            let material = message.material(record);
            state.started[record] = material != NO_MATERIAL
                && message.tier(record) != NO_TIER
                && materials.sound_id(material) != -1;
        }
        // `sub_824D2318`'s tail: a component that started nothing deactivates its state again.
        if !state.started.iter().any(|started| *started) {
            state.active = false;
            state.message = None;
        }
        slot
    }

    /// Release a slot's voice record, as retail does when a voice finishes. When both go idle the
    /// state deactivates, exactly as `sub_824D2318`'s tail does.
    pub(crate) fn release(&mut self, slot: usize, record: usize) {
        let state = &mut self.slots[slot];
        state.started[record] = false;
        if !state.started.iter().any(|started| *started) {
            state.active = false;
            state.message = None;
        }
    }

    /// `sub_824D1E00`, for every slot: `(controller key, input id, value)`.
    ///
    /// Retail rewrites both inputs from zero on every frame before deciding whether to raise them,
    /// so the zeroes are emitted too and in the original's order — this is a continuously driven
    /// pair, not a one-shot, which is the whole reason a landing's weight can vary while it rings.
    pub(crate) fn drive(&self) -> Vec<(u32, u32, u32)> {
        let mut writes = Vec::with_capacity(SLOT_COUNT * 2);
        for (index, slot) in self.slots.iter().enumerate() {
            let key = controller_key(index);
            writes.push((key, GATE_INPUT, 0));
            writes.push((key, WEIGHT_INPUT, 0));
            if !slot.active || !slot.started.iter().any(|started| *started) {
                continue;
            }
            let Some(message) = slot.message else { continue };
            writes.push((key, GATE_INPUT, FULL_SCALE));
            writes.push((key, WEIGHT_INPUT, contact_weight(message.tier_a, message.tier_b)));
        }
        writes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn materials() -> CollisionMaterials {
        let mut category = [0u8; MATERIAL_COUNT];
        // Enough of the real spread to exercise both output pickers.
        category[10] = 1;
        category[7] = 3;
        category[5] = 4;
        category[60] = 9;
        CollisionMaterials::from_categories(category)
    }

    fn message(material_a: i32, material_b: i32, tier_a: i32, tier_b: i32) -> ContactMessage {
        ContactMessage { material_a, material_b, tier_a, tier_b, ..ContactMessage::default() }
    }

    #[test]
    fn the_ten_controller_keys_are_the_ones_the_mixmap_holds() {
        // `mixmap_dump` prints exactly these for SFXObj_Collision #0..#9.
        assert_eq!(controller_key(0), 0x4003_0000);
        assert_eq!(controller_key(1), 0x4003_0800);
        assert_eq!(controller_key(9), 0x4003_4800);
    }

    #[test]
    fn the_weight_treats_three_as_no_tier_and_otherwise_takes_the_larger() {
        // One word absent: the other decides.
        assert_eq!(contact_weight(NO_TIER, 2), 32_767);
        assert_eq!(contact_weight(1, NO_TIER), 20_000);
        // Neither absent: the larger wins.
        assert_eq!(contact_weight(0, 1), 20_000);
        assert_eq!(contact_weight(2, 1), 32_767);
        // Anything that is not 1 or 2 — including both words absent — is the floor.
        assert_eq!(contact_weight(0, 0), 10_000);
        assert_eq!(contact_weight(NO_TIER, NO_TIER), 10_000);
    }

    #[test]
    fn the_output_pickers_diverge_on_a_material_past_the_table() {
        let m = materials();
        // `sub_824D20E8`'s jump table, category 6 → 12 is the one that is out of order.
        assert_eq!(m.level_output(0), 13);
        assert_eq!(m.level_output(10), 14);
        assert_eq!(m.level_output(7), 16);
        assert_eq!(m.level_output(60), 21);
        // 143 forces category 8 here …
        assert_eq!(m.level_output(NO_MATERIAL), 20);
        // … but takes the category-9 branch there. The two are not the same test.
        assert_eq!(m.pitch_output(NO_MATERIAL), 22);
        assert_eq!(m.pitch_output(60), 22);
        assert_eq!(m.pitch_output(10), 1);
    }

    #[test]
    fn a_negative_material_is_category_eight() {
        assert_eq!(materials().category(-1), NEGATIVE_MATERIAL_CATEGORY);
    }

    #[test]
    fn material_ninety_four_is_the_silent_one() {
        let m = materials();
        // The only `-1` in the retail table, and the same slot `SurfaceMap` clamps to.
        assert_eq!(m.sound_id(94), -1);
        assert_ne!(m.sound_id(0), -1);
        assert_eq!(m.sound_id(NO_MATERIAL), -1);
        assert_eq!(m.sound_id(-1), -1);
    }

    #[test]
    fn the_router_reuses_the_least_recently_used_slot() {
        let m = materials();
        let mut states = CollisionStates::new();
        // Ten posts fill every slot in order, because every stamp starts at 0 and ties go to the
        // first slot.
        for expected in 0..SLOT_COUNT {
            assert_eq!(states.post(message(0, 1, 0, 0), &m), expected);
        }
        // The eleventh comes back round to slot 0, the oldest stamp.
        assert_eq!(states.post(message(0, 1, 0, 0), &m), 0);
        assert_eq!(states.post(message(0, 1, 0, 0), &m), 1);
    }

    #[test]
    fn a_post_evicts_the_message_the_slot_was_holding() {
        let m = materials();
        let mut states = CollisionStates::new();
        let first = message(0, 1, 0, 0);
        let second = message(10, 7, 1, 2);
        for _ in 0..SLOT_COUNT {
            states.post(first, &m);
        }
        assert_eq!(states.slots()[0].message, Some(first));
        states.post(second, &m);
        assert_eq!(states.slots()[0].message, Some(second));
    }

    #[test]
    fn a_message_with_no_playable_material_leaves_its_state_inactive() {
        let m = materials();
        let mut states = CollisionStates::new();
        // Both records refused: material 143 on one side, tier 3 on the other.
        let slot = states.post(message(NO_MATERIAL, 1, 0, NO_TIER), &m);
        assert_eq!(states.slots()[slot].started, [false, false]);
        assert!(!states.slots()[slot].active);
        // And the silent material is refused the same way.
        let slot = states.post(message(94, NO_MATERIAL, 0, NO_TIER), &m);
        assert_eq!(states.slots()[slot].started, [false, false]);
        assert!(!states.slots()[slot].active);
    }

    #[test]
    fn the_drive_zeroes_every_slot_and_raises_only_the_live_ones() {
        let m = materials();
        let mut states = CollisionStates::new();
        let slot = states.post(message(0, 1, 1, NO_TIER), &m);
        let writes = states.drive();
        let key = controller_key(slot);
        // Every slot is zeroed first, live or not.
        assert_eq!(writes.iter().filter(|(_, id, v)| *id == GATE_INPUT && *v == 0).count(), SLOT_COUNT);
        // The live slot then raises the gate and the weight, in that order, after its own zeroes.
        let mine: Vec<_> = writes.iter().filter(|(k, ..)| *k == key).copied().collect();
        assert_eq!(
            mine,
            vec![(key, GATE_INPUT, 0), (key, WEIGHT_INPUT, 0), (key, GATE_INPUT, FULL_SCALE), (key, WEIGHT_INPUT, 20_000)]
        );
        // A slot nobody posted to is left at zero.
        let idle = controller_key(SLOT_COUNT - 1);
        let theirs: Vec<_> = writes.iter().filter(|(k, ..)| *k == idle).copied().collect();
        assert_eq!(theirs, vec![(idle, GATE_INPUT, 0), (idle, WEIGHT_INPUT, 0)]);
    }

    /// Pins the categories against the owner's real vault rather than a synthetic table: the
    /// baked keys must actually resolve, and the values must be the `0..=9` the jump table
    /// indexes. Ignored because it needs the installation.
    ///
    ///     cargo test -p skate-game --bin skate3rust -- --ignored the_vault_resolves --nocapture
    #[test]
    #[ignore = "needs the owner's assets"]
    fn the_vault_resolves_every_material_category() {
        let assets = std::path::Path::new(
            r"C:\s3\installations\70eda9dc4644496d81ae73af95ff4285\assets",
        );
        let vault = Collections::load(assets).expect("vault");
        let (materials, unresolved) = CollisionMaterials::load(&vault);
        // Exactly one material has no record: 94, whose key is all zeroes and whose sound id is
        // the table's only `-1`. (A JSON export of the vault appears to be missing five further
        // records — 77, 87, 92, 97, 98 — but the vault itself has them, which is why this test
        // reads the vault and not the export.)
        assert_eq!(unresolved, 1, "unexpected number of materials without a vault record");
        assert_eq!(materials.sound_id(94), -1);
        let mut seen = [0usize; 10];
        for material in 0..MATERIAL_COUNT as i32 {
            let category = materials.category(material);
            assert!(category <= 9, "material {material} has category {category}");
            seen[category as usize] += 1;
        }
        // Every category is used, so the whole jump table is reachable from real data.
        assert!(seen.iter().all(|count| *count > 0), "unused categories: {seen:?}");
        println!("category histogram {seen:?}, {unresolved} materials without a record");
    }

    #[test]
    fn releasing_both_records_deactivates_the_state() {
        let m = materials();
        let mut states = CollisionStates::new();
        let slot = states.post(message(0, 10, 0, 1), &m);
        assert_eq!(states.slots()[slot].started, [true, true]);
        states.release(slot, 0);
        assert!(states.slots()[slot].active);
        states.release(slot, 1);
        assert!(!states.slots()[slot].active);
        assert_eq!(states.slots()[slot].message, None);
        // And it stops driving its controller.
        let key = controller_key(slot);
        let mine: Vec<_> = states.drive().iter().filter(|(k, ..)| *k == key).copied().collect();
        assert_eq!(mine, vec![(key, GATE_INPUT, 0), (key, WEIGHT_INPUT, 0)]);
    }
}
