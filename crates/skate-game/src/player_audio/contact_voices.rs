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
use super::components::contacts::{ContactSound, ContactVoices, SurfaceMap, VoiceRequest};

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
}

impl ContactVoicePlayer {
    pub(crate) fn new(assets: &std::path::Path, cache: Option<&std::path::Path>) -> Result<Self, String> {
        let vault = Collections::load(assets)?;
        Ok(Self {
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
            let Some((bank, sample, mut bus)) = self.selection(&play, audio) else {
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
            let mut handles = Vec::with_capacity(members.len());
            for member in members {
                match runtime.play_oneshot(&OneshotVoice {
                    sample: base + member.stream_offset,
                    gain: member.values.gain,
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
        for live in &mut self.live {
            for handle in &mut live.handles {
                runtime.tick_oneshot(handle, dt).map_err(|e| e.to_string())?;
            }
        }
        // A play whose voices have all finished keeps no handles; retail's slot still holds its
        // container until the component frees it, so the entry stays until `free`.
        Ok(())
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
            // Pops off (see `pops_enabled`), and the two paths that are a different subsystem.
            ContactSound::Pop | ContactSound::PopRoll | ContactSound::GrindOnset => None,
        }
    }
}
