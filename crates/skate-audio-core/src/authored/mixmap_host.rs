//! The MixMap inside [`AuthoredRuntime`]'s guest: load, per-frame tick, controller lookup, input
//! writes and the owner readers (vfunc 52/56/60/64).
//!
//! The MixMap lives in its own guest span at [`MIXMAP_SPACE`], whose first 256 bytes are a header
//! (manager, host, file data, heap cursor) so no field is added to the runtime itself. Everything
//! inside the span is built by [`crate::mixmap::load`] exactly as `sub_82484FE8` builds it; the
//! image tables it reads come from the runtime's guest image.
//!
//! Per game frame, in this order (see `crate::mixmap` for the evidence):
//! 1. the audio-state bridge, then [`crate::mixmap::inputs`]'s writers for the state and position
//!    controllers ([`AuthoredRuntime::mixmap_set`] / [`AuthoredRuntime::mixmap_set_float`]);
//! 2. [`AuthoredRuntime::mixmap_tick`];
//! 3. the components' updates, reading this evaluation with the readers below;
//! 4. inputs a component writes during its update are consumed by the next tick.

use super::AuthoredRuntime;
use crate::mixmap::{self, KeyedListener, controller};
use crate::patch::BumpHeap;
use crate::{Error, Result};

/// Guest base of the MixMap span (after the graph heap, `0x6400_0000 + 64 MB`).
pub const MIXMAP_SPACE: u32 = 0x6800_0000;
/// The span's size: the build uses about 230 KB of it.
pub const MIXMAP_BYTES: u32 = 0x0040_0000;
const MAGIC: u32 = 0x4D58_4D50; // "MXMP"
const HEADER: u32 = 0x100;

impl AuthoredRuntime {
    /// `sub_82484FE8`'s MixMap half: build the host from `MixMapSK8.mxb` (see
    /// `PlayerAudioCatalog::mixmap`). The instance counts per SFX slot are the retail session's
    /// ([`mixmap::RETAIL_INSTANCE_COUNTS`]). Replaces any MixMap loaded before.
    pub fn load_mixmap(&mut self, mxb: &[u8]) -> Result<()> {
        let g = &mut self.guest;
        g.put(MIXMAP_SPACE, vec![0; MIXMAP_BYTES as usize]);
        let mut heap = BumpHeap {
            next: MIXMAP_SPACE + HEADER,
            end: MIXMAP_SPACE + MIXMAP_BYTES,
        };
        let mut listener = KeyedListener::retail();
        let mm = mixmap::load(g, &mut heap, &mut listener, mxb)?;
        g.set_u32(MIXMAP_SPACE + 4, mm.manager)?;
        g.set_u32(MIXMAP_SPACE + 8, mm.host)?;
        g.set_u32(MIXMAP_SPACE + 12, mm.data)?;
        g.set_u32(MIXMAP_SPACE + 16, heap.next)?;
        g.set_u32(MIXMAP_SPACE, MAGIC)?;
        Ok(())
    }

    /// Whether [`Self::load_mixmap`] has run.
    pub fn has_mixmap(&self) -> bool {
        self.guest.u32(MIXMAP_SPACE).is_ok_and(|m| m == MAGIC)
    }

    fn mixmap_header(&self) -> Result<(u32, u32)> {
        if !self.has_mixmap() {
            return Err(Error::new(MIXMAP_SPACE, "MixMap not loaded"));
        }
        Ok((self.guest.u32(MIXMAP_SPACE + 4)?, self.guest.u32(MIXMAP_SPACE + 8)?))
    }

    /// The manager's per-frame tick (`sub_8294BAE8` → host slot 2, `sub_8294F5E8`).
    pub fn mixmap_tick(&mut self, dt: f32) -> Result<()> {
        let (manager, _) = self.mixmap_header()?;
        mixmap::tick(&mut self.guest, manager, f64::from(dt))
    }

    /// The controller for a key (`mixmap::key(kind, slot, group, object)`), as `sub_8294CB48` finds
    /// it; `None` before [`Self::load_mixmap`] or for a key the file never names.
    pub fn mixmap_controller(&self, key: u32) -> Option<u32> {
        let (_, host) = self.mixmap_header().ok()?;
        mixmap::find_controller(&self.guest, host, key).ok().flatten()
    }

    /// Controller slot 8 (`sub_8294BC50`): `[ctrl+8][id] = value`.
    pub fn mixmap_set(&mut self, ctrl: u32, id: u32, value: u32) -> Result<()> {
        controller::set(&mut self.guest, ctrl, id, value)
    }

    /// Controller slot 4 (`sub_8294BC68`): `fctiwz` then slot 8.
    pub fn mixmap_set_float(&mut self, ctrl: u32, id: u32, value: f32) -> Result<()> {
        controller::set_float(&mut self.guest, ctrl, id, f64::from(value))
    }

    /// Store the float's bits unconverted, as the position controllers' writers do for ids 0, 1,
    /// 13 and 14 (`stfs` then `lwz` into slot 8, e.g. `sub_824AEE60`'s `lwz r5,96(r31)`).
    pub fn mixmap_set_bits(&mut self, ctrl: u32, id: u32, value: f32) -> Result<()> {
        controller::set(&mut self.guest, ctrl, id, value.to_bits())
    }

    /// Controller slot 12 (`sub_8294BCA0`).
    pub fn mixmap_get(&self, ctrl: u32, id: u32) -> Result<u32> {
        controller::get(&self.guest, ctrl, id)
    }

    /// Owner vfunc 52 (`sub_824C2870`): the output half, 16 bits.
    pub fn mixmap_raw(&self, ctrl: u32, id: u32) -> u32 {
        controller::read_u16(&self.guest, ctrl, id).unwrap_or(0)
    }

    /// Owner vfunc 56 (`sub_824C5910`): cents → `2^(c/1200) × 4096`.
    pub fn mixmap_pitch(&self, ctrl: u32, id: u32) -> u32 {
        controller::read_pitch(&self.guest, ctrl, id).unwrap_or(0)
    }

    /// Owner vfunc 60/64 (`sub_824AF240`): the output half `& 0x7FFF`.
    pub fn mixmap_level(&self, ctrl: u32, id: u32) -> u32 {
        controller::read_gain(&self.guest, ctrl, id).unwrap_or(0)
    }

    /// Controller slot 24 (`sub_8294BDB0`): output word 15 = 1.
    pub fn mixmap_enable(&mut self, ctrl: u32) -> Result<()> {
        controller::enable(&mut self.guest, ctrl)
    }

    /// Controller slot 20 (`sub_8294BD98`): output word 15 = 0 (the evaluation then writes each
    /// block's first output as its "off" value).
    pub fn mixmap_disable(&mut self, ctrl: u32) -> Result<()> {
        controller::disable(&mut self.guest, ctrl)
    }
}
