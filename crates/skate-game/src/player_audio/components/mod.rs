//! The retail player-sound components, one per game-side sound object.
//!
//! Each mirrors its retail class: vtable slot 9 (`process`, the `+36` tick) posts and releases
//! messages, slot 10 (`update`, the `+40` tick) rewrites the held packets and redelivers them.
//! Gameplay words come from the retail audio state ([`AudioState`]), gain/pitch/pan words from the
//! component's MixMap controller ([`Controls`]), tuning from the owner's AttribSys vault. Word
//! formulas are in [`words`], each checked against the retail recomp capture.
//!
//! Retail components read the audio state the bridge wrote on the previous frame (verified: every
//! capture-checked formula matches with a one-frame lag, and only then), so the worker hands them
//! last tick's state.

pub(crate) mod board;
pub(crate) mod clothing;
pub(crate) mod contacts;
pub(crate) mod footsteps;
pub(crate) mod grind;
pub(crate) mod seams;
pub(crate) mod speed;
pub(crate) mod treatment;
pub(crate) mod tricks;
pub(crate) mod words;

use skate_audio_core::authored::AuthoredRuntime;

use super::audio_state::AudioState;

/// A component's reads of its MixMap controller's output table (`[[owner+12]+12]`).
pub(crate) trait Controls {
    /// Owner vfunc 52, `sub_824C2870`: the raw 16-bit output.
    fn raw(&self, id: u32) -> u32;
    /// Owner vfunc 56, `sub_824C5910`: a cents output converted to `2^(c/1200) × 4096`.
    fn pitch(&self, id: u32) -> i32;
    /// Owner vfuncs 60/64, `sub_824AF240`: the 15-bit output.
    fn level(&self, id: u32) -> u32;
}

/// What one component tick sees.
pub(crate) struct Tick<'a> {
    pub runtime: &'a mut AuthoredRuntime,
    /// The bridge's state from the previous frame (see the module note).
    pub audio: &'a AudioState,
    pub controls: &'a dyn Controls,
    /// Game frame time in seconds.
    pub dt: f32,
    pub tick: u64,
}

pub(crate) trait Component {
    /// Vtable slot 9 / `+36`: post, release and trigger logic.
    fn process(&mut self, tick: &mut Tick) -> Result<(), String>;
    /// Vtable slot 10 / `+40`: rewrite and redeliver the held packets.
    fn update(&mut self, tick: &mut Tick) -> Result<(), String>;
}

/// Release a held message, if any.
pub(crate) fn release(runtime: &mut AuthoredRuntime, holder: &mut Option<u32>) -> Result<(), String> {
    if let Some(handle) = holder.take() {
        runtime.release(handle).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn post(runtime: &mut AuthoredRuntime, object: &str, words: &[u32]) -> Result<u32, String> {
    runtime.post(object, words).map_err(|error| error.to_string())
}

pub(crate) fn redeliver(runtime: &mut AuthoredRuntime, handle: u32, words: &[u32]) -> Result<(), String> {
    runtime.redeliver(handle, words).map_err(|error| error.to_string())
}
