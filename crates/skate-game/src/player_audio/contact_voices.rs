//! The Contacts component's one-shot bank voices: wheel pops and the landing impact.
//!
//! `ContactsOwner` decides *when* a contact sound plays (the retail edges and selector ladders) but
//! cannot reach the runtime from [`ContactVoices`], so it records its plays here and the worker
//! drains them: the request's selector and the audio state pick a Splice bank sample
//! (`skate_data::audio::splice`), the sample resolves to its layered members, and each member is
//! opened as a one-shot voice (`AuthoredRuntime::play_oneshot`). One retail voice is therefore a
//! set of handles, held and freed together the way `sub_824836B8` frees the container's children.
//!
//! Not routed here: `ContactSound::GrindOnset` and `ContactSound::PopRoll`. Grind onset and the
//! rolling/scrape contacts are a different subsystem (`sub_82496C58`'s material-pair gain and
//! `sub_82486EF0`'s request to the contact-sound manager at `[manager+668]`), and the pop-roll
//! layer's bank is not resolved by the ported selection. Both are dropped with a label rather
//! than played from an invented sample.

use skate_audio_core::authored::{
    AuthoredRuntime,
    oneshot::{OneshotBus, OneshotHandle, OneshotVoice, Rand},
};
use skate_data::audio::splice::{LandingTuning, PopsTuning, SpliceBanks, SpliceState, surface_category};
use skate_data::collections::Collections;

use super::audio_state::AudioState;
use super::collision_states::{CollisionMaterials, CollisionStates, ContactMessage};
use super::components::contacts::{ContactSound, ContactVoices, SurfaceMap, VoiceRequest};

/// Contacts controller output 15 at landing class 2, measured from the real MixMap under the
/// retail pre-roll (`skate-audio-core` example `contacts_input_probe`): classes 0/1/2 give
/// 2584/3103/3650, a 3.0 dB spread. Class 2 is the reference so the loudest landing keeps the
/// level `player-audio-retail-drivers.md` §8 measured against the recomp.
const LANDING_SEND_MAXIMUM: f32 = 3650.0;
/// The send is an environment level as well as a class level, so it can legitimately sit low.
/// Retail's own class-0 reading is 0.708 of the maximum; this floor keeps an unusual environment
/// from muting a landing outright while still letting the class spread through.
const LANDING_SEND_FLOOR: f32 = 0.5;

/// One recorded play, in the order `ContactsOwner` made it.
struct Queued {
    id: u32,
    sound: ContactSound,
    request: VoiceRequest,
}

/// A handle both the component and the worker hold: the component records plays through its
/// [`ContactVoices`] impl, the worker drains them. The whole player-sound path lives on the audio
/// worker thread, so a plain `Rc<RefCell<..>>` is enough.
#[derive(Clone, Default)]
pub(crate) struct SharedContactVoices(std::rc::Rc<std::cell::RefCell<QueuedContactVoices>>);

impl ContactVoices for SharedContactVoices {
    fn play(&mut self, sound: ContactSound, request: &VoiceRequest) -> Option<u32> {
        self.0.borrow_mut().play(sound, request)
    }

    fn free(&mut self, handle: u32) {
        self.0.borrow_mut().free(handle);
    }
}

/// The recorded plays. Opaque ids; [`ContactVoicePlayer`] maps them to the voices they opened.
#[derive(Default)]
pub(crate) struct QueuedContactVoices {
    queued: Vec<Queued>,
    freed: Vec<u32>,
    next: u32,
}

impl ContactVoices for QueuedContactVoices {
    fn play(&mut self, sound: ContactSound, request: &VoiceRequest) -> Option<u32> {
        self.next += 1;
        let id = self.next;
        self.queued.push(Queued { id, sound, request: *request });
        Some(id)
    }

    fn free(&mut self, handle: u32) {
        self.freed.push(handle);
    }
}

/// The voices one recorded play opened.
struct Live {
    id: u32,
    handles: Vec<OneshotHandle>,
}

/// Turns the component's recorded plays into real voices and ticks them.
pub(crate) struct ContactVoicePlayer {
    banks: SpliceBanks,
    pops: PopsTuning,
    landing: LandingTuning,
    surfaces: SurfaceMap,
    state: SpliceState,
    rand: Rand,
    live: Vec<Live>,
    reported: std::collections::HashSet<String>,
    /// The takeoff pops. Retail's metered takeoff is +17 dB over its rolling bed while this
    /// engine's was +5.5 dB without them, so they are on by default; `SKATE_AUDIO_CONTACT_POPS=0`
    /// turns them off. Their MixMap-driven environment send still uses `RETAIL_POPS_LEVEL`,
    /// because the Contacts inputs that drive it are not written yet.
    pops_enabled: bool,
    /// The pops' owner-local six-channel send bus (`sub_82488DD0`), built on first use.
    pops_send: Option<u32>,
    /// `CSTATEMGR_Collision` and its per-material table: the subsystem the grind onset posts to.
    collision: CollisionStates,
    collision_materials: CollisionMaterials,
}

impl ContactVoicePlayer {
    pub(crate) fn new(assets: &std::path::Path, cache: Option<&std::path::Path>) -> Result<Self, String> {
        let vault = Collections::load(assets)?;
        // `sub_82496FD0` resolves a material's category through the vault one lookup at a time;
        // doing all 143 once here is the same table. Only material 94 — the silent slot, whose
        // key is all zeroes — is expected to be missing, so anything else is worth saying.
        let (collision_materials, unresolved) = CollisionMaterials::load(&vault);
        if unresolved > 1 {
            eprintln!(
                "SKATE_PLAYER_AUDIO contact_voice_unavailable {unresolved} collision materials \
                 have no vault record; their category falls back to retail's default record"
            );
        }
        Ok(Self {
            collision: CollisionStates::new(),
            collision_materials,
            banks: SpliceBanks::load(
                &assets.join("private/stock/data/audio/audiofiles.big"),
                // The DLC bank is absent from a stock installation; only the stock one is required.
                &[
                    skate_data::audio::splice::COLLISIONS_BANK,
                    skate_data::audio::splice::DLC_COLLISIONS_BANK,
                ],
                &[skate_data::audio::splice::COLLISIONS_BANK],
            )
            .map_err(|error| error.to_string())?,
            pops: PopsTuning::load(&vault).map_err(|error| error.to_string())?,
            landing: LandingTuning::load(&vault).map_err(|error| error.to_string())?,
            surfaces: SurfaceMap::load(&vault)?,
            state: SpliceState::default(),
            rand: Rand::new(cache.map_or(1, |_| 1)),
            live: Vec::new(),
            reported: std::collections::HashSet::new(),
            pops_enabled: std::env::var("SKATE_AUDIO_CONTACT_POPS").map_or(true, |v| v != "0"),
            pops_send: None,
        })
    }

    /// Open everything the component recorded this frame, free what it released, and advance the
    /// voices already playing.
    pub(crate) fn drain(
        &mut self,
        runtime: &mut AuthoredRuntime,
        shared: &SharedContactVoices,
        audio: &AudioState,
        dt: f32,
    ) -> Result<(), String> {
        let (queued, freed) = {
            let mut voices = shared.0.borrow_mut();
            (
                std::mem::take(&mut voices.queued),
                std::mem::take(&mut voices.freed),
            )
        };
        for id in freed {
            if let Some(index) = self.live.iter().position(|live| live.id == id) {
                let mut live = self.live.swap_remove(index);
                for handle in &mut live.handles {
                    runtime.release_oneshot(handle).map_err(|e| e.to_string())?;
                }
            }
        }
        for play in queued {
            // `sub_824BB0E0`'s tail posts to the contact-sound manager rather than starting a
            // bank voice, so the grind onset leaves this path here.
            if matches!(play.sound, ContactSound::GrindOnset) {
                self.post_grind_onset(&play.request);
                continue;
            }
            let Some((bank, sample, mut bus)) = self.selection(&play, audio) else {
                // The two unported subsystem paths dropped silently, which made a
                // rail landing look like nothing had even been requested. Say so
                // once per run; the other `None`s are ordinary (pops disabled, or
                // a landing with no wheel actually down).
                if matches!(play.sound, ContactSound::PopRoll) {
                    self.report(&format!(
                        "{:?} needs the contact-sound manager ([manager+668], `sub_82486EF0`); not ported, so this contact is silent",
                        play.sound
                    ));
                }
                continue;
            };
            // `sub_824B9CC8` routes the pops through the owner's own send bus, whose first send
            // carries the environment level the owner reads as level(14).
            if matches!(play.sound, ContactSound::Pop) {
                let send = match self.pops_send {
                    Some(send) => send,
                    None => {
                        match runtime.build_owner_send(
                            self.pops.bus,
                            skate_audio_core::authored::oneshot::RETAIL_POPS_LEVEL,
                        ) {
                            Ok(send) => {
                                self.pops_send = Some(send);
                                send
                            }
                            Err(error) => {
                                self.report(&format!("pops send bus: {error}"));
                                continue;
                            }
                        }
                    }
                };
                bus = OneshotBus::Module(send);
            }
            // A contact sound that cannot resolve or play must not take the whole player-sound
            // worker down with it: report it once and carry on with the rest of the mix.
            let members = match self.banks.resolve(bank, sample, &mut self.state, &mut self.rand) {
                Ok(members) => members,
                Err(error) => {
                    self.report(&format!("{bank} sample {sample:#x}: {error}"));
                    continue;
                }
            };
            let Some(base) = runtime.bank_base(bank) else {
                self.report(&format!("Splice bank {bank} is not installed"));
                continue;
            };
            // Retail's landing level rides the owner send (Contacts output 15); see
            // `selection`. Taken relative to the class-2 maximum so the loudest landing keeps
            // the level §8 measured and the lighter classes drop by retail's own ratios.
            let send_scale = match play.request.send_level {
                Some(level) if matches!(play.sound, ContactSound::LandingClass) => {
                    (level as f32 / LANDING_SEND_MAXIMUM).clamp(LANDING_SEND_FLOOR, 1.0)
                }
                _ => 1.0,
            };
            let mut handles = Vec::with_capacity(members.len());
            for member in members {
                match runtime.play_oneshot(&OneshotVoice {
                    sample: base + member.stream_offset,
                    gain: member.values.gain * send_scale,
                    pitch: member.values.pitch,
                    delay: member.values.delay,
                    pan: member.pan,
                    bus,
                }) {
                    Ok(handle) => handles.push(handle),
                    Err(error) => self.report(&format!("{bank} sample {sample:#x} member: {error}")),
                }
            }
            self.live.push(Live { id: play.id, handles });
        }
        // `sub_824D1E00` runs on every `SFXObj_Collision` every frame, whether or not anything was
        // posted this one: it rewrites both inputs from zero and only then decides to raise them.
        for (key, id, value) in self.collision.drive() {
            runtime.mixmap_apply(key, &[(id, value)]).map_err(|e| e.to_string())?;
        }
        for live in &mut self.live {
            for handle in &mut live.handles {
                runtime.tick_oneshot(handle, dt).map_err(|e| e.to_string())?;
            }
        }
        // A play whose voices have all finished keeps no handles; retail's slot still holds its
        // container until the component frees it, so the entry stays until `free`.
        Ok(())
    }

    /// `sub_824BB0E0`'s tail: build the 48-byte message and hand it to `CSTATEMGR_Collision`.
    ///
    /// The argument order is the lifted one — `sub_82486EF0(this, family_base, material, tier,
    /// tier, &position, level_a, level_b, flags…)` — and note the grind onset passes **the same
    /// tier word twice** (`mr r6,r30` / `mr r7,r30`), so `contact_weight` sees a matched pair and
    /// the onset lands on the 10000/20000 step rather than the 32767 one.
    ///
    /// Two message fields are deliberately left at zero, and neither is read by anything ported
    /// here: the `+0x10` world position (audio state `+48`, which only the spatialisation in the
    /// unported voice layer consumes) and the two `+0x20`/`+0x24` levels, which come from
    /// `sub_82496C58`'s per-material interpolation — decoded in `docs/engine-defects.md` #9 but
    /// not ported, and read only by the sample chooser.
    fn post_grind_onset(&mut self, request: &VoiceRequest) {
        let tier = request.tier.unwrap_or(0);
        let message = ContactMessage {
            material_a: request.family_base.unwrap_or(0) as i32,
            material_b: request.material.unwrap_or(0) as i32,
            tier_a: tier,
            tier_b: tier,
            ..ContactMessage::default()
        };
        self.collision.post(message, &self.collision_materials);
        // Until `sub_824965D0` → `sub_824967F8` are ported the slot drives its controller but
        // starts no sample, so the onset is still inaudible. Say so once rather than let a rail
        // landing look like it was never requested.
        self.report(
            "GrindOnset posts to CSTATEMGR_Collision, but the sample chooser \
             (`sub_824965D0` -> `sub_824967F8`) is not ported, so no voice starts yet",
        );
    }

    /// Report a contact-sound failure once per distinct message (they would otherwise repeat every
    /// landing).
    fn report(&mut self, message: &str) {
        if self.reported.insert(message.to_owned()) {
            eprintln!("SKATE_PLAYER_AUDIO contact_voice_unavailable {message}");
        }
    }

    /// `sub_824B9CC8`'s and `sub_824BA630`'s sample choice.
    fn selection(&self, play: &Queued, audio: &AudioState) -> Option<(&'static str, u16, OneshotBus)> {
        match play.sound {
            ContactSound::Pop if self.pops_enabled => {
                let class = usize::try_from(play.request.selector.unwrap_or(0)).unwrap_or(0);
                // `sub_824BA310`: the wheel material's AudioSurfaceMap lane +8 plus the soft-wheel
                // test (`sub_82494D78`).
                let material = audio.wheel_material_620[0];
                let lane = if material >= 143 {
                    0
                } else {
                    self.surfaces.lookup(material as i32, 8) as u32
                };
                let category = surface_category(lane, audio.soft_wheels_684 != 0);
                let (bank, sample) = self.pops.sample(class, category, false)?;
                // Replaced with the owner send bus in `drain`.
                Some((bank, sample, OneshotBus::EqChain(self.pops.bus)))
            }
            ContactSound::Landing => {
                Some((self.landing.bank(), self.landing.sample, OneshotBus::Default))
            }
            // `sub_824B8D48` via `sub_824BA3F0`: index `3 × kind + class` with `kind = 0` for a
            // landing, and the mode is the surface category only for a class-2 landing.
            //
            // The class is the largest `+448` bucket over the wheels that are in contact
            // (`+464`), exactly as `sub_824BA630`'s tail computes it for MixMap input 2. Retail
            // derives those buckets from time in air, thresholded at 0.62 s and 1.00 s.
            ContactSound::LandingClass => {
                let (mut class, mut landed) = (0, false);
                for wheel in 0..4 {
                    if audio.wheel_landed_464[wheel] {
                        landed = true;
                        class = class.max(audio.wheel_landing_bucket_448[wheel]);
                    }
                }
                if !landed {
                    return None;
                }
                let material = audio.wheel_material_620[0];
                let lane = if material >= 143 {
                    0
                } else {
                    self.surfaces.lookup(material as i32, 8) as u32
                };
                let category = surface_category(lane, audio.soft_wheels_684 != 0);
                let sample = self.landing.class_sample(0, class, category)?;
                // Retail routes this voice through the owner's send bus, whose level is the
                // Contacts controller's output 15 (`sub_824B8D48` @ 0x824B8E88 →
                // `sub_82488DD0`). Output 15 is driven by controller input 2, which
                // `ContactsInputs::process` already writes from the landing class — so retail's
                // landing level really does vary with the drop, and this engine was throwing that
                // away by sending the dry voice to the default bus.
                //
                // Probing the real MixMap under the retail pre-roll
                // (`skate-audio-core` example `contacts_input_probe`) gives output 15 =
                // 2584 / 3103 / 3650 for classes 0 / 1 / 2: a 3.0 dB spread, class 0 to class 2.
                //
                // The absolute level is left where §8 measured it. `CONTACT_TRIM` was calibrated
                // with the send absent, so re-applying the raw send level would drop every landing
                // ~19 dB. Instead the level is taken relative to its class-2 maximum, which keeps
                // the loudest landing exactly where it measures today and lets the lighter classes
                // fall back by retail's own ratios. That is the part the owner could hear missing:
                // every landing arriving at the same level.
                Some((self.landing.bank(), sample, OneshotBus::Default))
            }
            // Pops off (see `pops_enabled`), and the two paths that are a different subsystem.
            ContactSound::Pop | ContactSound::PopRoll | ContactSound::GrindOnset => None,
        }
    }
}
