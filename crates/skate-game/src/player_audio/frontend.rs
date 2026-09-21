//! The front-end one-shots, which today means the session marker's three cellphone sounds.
//!
//! Retail plays these through `GlobalFEPlaySound` (`sub_825DFAF0`): `PlayerUI::UpdateSessionMarker`
//! (`sub_82898FC8`) builds a 64-bit AttribSys id inline and calls it, and that function looks the
//! id up in the vault and hands it to the front-end sound manager. `session_marker` already
//! emitted those three ids; the message simply had no consumer, which is why the marker has been
//! silent. It now travels as [`FrontEndSoundRequest`](crate::skate_audio::FrontEndSoundRequest).
//!
//! The ids are the AttribSys hashes of the vault keys, confirmed by hashing the names back:
//!
//! | id | `fe` key | sample |
//! |---|---|---|
//! | `0x0D6C88A3B91C828F` | `cellphone_place_marker` | 237 |
//! | `0x7F135F9FD28F7F21` | `cellphone_goto_marker` | 236 |
//! | `0x66B3AFE3B602918C` | `cellphone_marker_error` | 209 |
//!
//! Each record is class `fe`, parent `fe_sfx`, and carries its sample in a field whose type name
//! is `sk8_menu` -- the same "a field's type name is its bank" convention the collision materials
//! use -- plus a `Volume`. Nothing here is hard-coded: the sample and gain are read from the
//! owner's own vault at load, so a modified install gets its own values.
//!
//! Not ported: retail gates `GlobalFEPlaySound` on three collection words (a front-end enable
//! state this engine has no equivalent for), so these always play.
use skate_data::collections::Collections;

/// `Volume`, and the sample field whose type name is the bank.
const VOLUME_FIELD: &str = "Hash_875BA75341DC8391";
const SAMPLE_FIELD: &str = "Hash_8FCC7EF9B9208858";
const FE_CLASS: &str = "fe";

/// One front-end sound, keyed by the id `session_marker` emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrontEndSound {
    /// `cellphone_place_marker`: the session marker was set.
    PlaceMarker,
    /// `cellphone_goto_marker`: the session returned to the marker.
    GotoMarker,
    /// `cellphone_marker_error`: the placement was refused.
    MarkerError,
}

impl FrontEndSound {
    /// The AttribSys ids `PlayerUI::UpdateSessionMarker` builds inline.
    pub(crate) fn from_id(id: u64) -> Option<Self> {
        match id {
            0x0d6c_88a3_b91c_828f => Some(Self::PlaceMarker),
            0x7f13_5f9f_d28f_7f21 => Some(Self::GotoMarker),
            0x66b3_afe3_b602_918c => Some(Self::MarkerError),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::PlaceMarker => "cellphone_place_marker",
            Self::GotoMarker => "cellphone_goto_marker",
            Self::MarkerError => "cellphone_marker_error",
        }
    }
}

/// What one front-end sound plays: a Splice sample in `sk8_menu.bnk`, and its authored gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrontEndVoice {
    pub sample: u16,
    pub gain: f32,
}

/// The three session-marker sounds as the owner's vault holds them.
#[derive(Clone, Debug, Default)]
pub(crate) struct FrontEndSounds {
    place: Option<FrontEndVoice>,
    goto: Option<FrontEndVoice>,
    error: Option<FrontEndVoice>,
}

impl FrontEndSounds {
    /// A record this installation does not carry stays `None` and simply makes no sound, which is
    /// how retail's own lookup behaves when the id resolves to nothing.
    pub(crate) fn load(vault: &Collections) -> Self {
        let read = |sound: FrontEndSound| -> Option<FrontEndVoice> {
            let key = sound.key();
            let sample = vault
                .field(FE_CLASS, key, SAMPLE_FIELD)
                .ok()
                .and_then(|field| u32::from_str_radix(field.data.trim(), 16).ok())
                .and_then(|value| u16::try_from(value).ok())
                .filter(|sample| *sample != 0)?;
            // Retail treats the volume as an ordinary authored gain; a record without one plays
            // at unity rather than silently.
            let gain = vault.float(FE_CLASS, key, VOLUME_FIELD).unwrap_or(1.0);
            Some(FrontEndVoice { sample, gain })
        };
        Self {
            place: read(FrontEndSound::PlaceMarker),
            goto: read(FrontEndSound::GotoMarker),
            error: read(FrontEndSound::MarkerError),
        }
    }

    pub(crate) fn voice(&self, sound: FrontEndSound) -> Option<FrontEndVoice> {
        match sound {
            FrontEndSound::PlaceMarker => self.place,
            FrontEndSound::GotoMarker => self.goto,
            FrontEndSound::MarkerError => self.error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three ids are the AttribSys hashes of the three vault keys. If either side is ever
    /// edited, this is what catches the mismatch — the ids are built inline in retail's code and
    /// cannot be re-derived from the vault alone.
    #[test]
    fn every_session_marker_id_maps_to_its_own_sound() {
        for (id, expected) in [
            (0x0d6c_88a3_b91c_828f, FrontEndSound::PlaceMarker),
            (0x7f13_5f9f_d28f_7f21, FrontEndSound::GotoMarker),
            (0x66b3_afe3_b602_918c, FrontEndSound::MarkerError),
        ] {
            assert_eq!(FrontEndSound::from_id(id), Some(expected));
        }
        assert_eq!(FrontEndSound::from_id(0), None);
        // Distinct keys, so two sounds can never collapse onto one record.
        let keys = [
            FrontEndSound::PlaceMarker.key(),
            FrontEndSound::GotoMarker.key(),
            FrontEndSound::MarkerError.key(),
        ];
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    /// Against the owner's real vault: all three must resolve, to the samples the `fe` records
    /// carry, out of the `sk8_menu` bank.
    ///
    ///     cargo test -p skate-game --bin skate3rust -- --ignored the_vault_resolves_every_session_marker --nocapture
    #[test]
    #[ignore = "needs the owner's assets"]
    fn the_vault_resolves_every_session_marker_sound() {
        let assets =
            std::path::Path::new("C:/s3/installations/70eda9dc4644496d81ae73af95ff4285/assets");
        let vault = Collections::load(assets).expect("vault");
        let sounds = FrontEndSounds::load(&vault);
        for (sound, sample) in [
            (FrontEndSound::PlaceMarker, 237),
            (FrontEndSound::GotoMarker, 236),
            (FrontEndSound::MarkerError, 209),
        ] {
            let voice = sounds
                .voice(sound)
                .unwrap_or_else(|| panic!("{sound:?} did not resolve"));
            assert_eq!(voice.sample, sample, "{sound:?}");
            assert!(voice.gain > 0.0, "{sound:?} gain {}", voice.gain);
            // The field's type name is the bank, which is what puts these in `sk8_menu.bnk`.
            let field = vault
                .field(FE_CLASS, sound.key(), SAMPLE_FIELD)
                .expect("sample field");
            assert_eq!(field.type_name, "sk8_menu", "{sound:?}");
        }
    }

    /// The bank half: `sk8_menu.bnk` must actually be in the archive and its three containers must
    /// resolve to real members. Without this the wiring would be silently inert -- an unresolvable
    /// optional bank is tolerated by design, so nothing else would complain.
    ///
    ///     cargo test -p skate-game --bin skate3rust -- --ignored the_menu_bank_resolves --nocapture
    #[test]
    #[ignore = "needs the owner's assets"]
    fn the_menu_bank_resolves_every_session_marker_sample() {
        use skate_audio_core::authored::oneshot::Rand;
        use skate_data::audio::splice::{SpliceBanks, SpliceState};
        let assets =
            std::path::Path::new("C:/s3/installations/70eda9dc4644496d81ae73af95ff4285/assets");
        let vault = Collections::load(assets).expect("vault");
        let sounds = FrontEndSounds::load(&vault);
        let bank = skate_data::audio::catalog::MENU_BANK;
        let banks = SpliceBanks::load(
            &assets.join("private/stock/data/audio/audiofiles.big"),
            &[bank],
            &[bank],
        )
        .expect("sk8_menu.bnk is in the archive");
        let mut state = SpliceState::default();
        let mut rand = Rand::new(1);
        for sound in [
            FrontEndSound::PlaceMarker,
            FrontEndSound::GotoMarker,
            FrontEndSound::MarkerError,
        ] {
            let voice = sounds.voice(sound).expect("resolved");
            let members = banks
                .resolve(bank, voice.sample, &mut state, &mut rand)
                .unwrap_or_else(|e| panic!("{sound:?} sample {:#x}: {e}", voice.sample));
            assert!(
                !members.is_empty(),
                "{sound:?} sample {:#x} resolved to no members",
                voice.sample
            );
            println!(
                "{sound:?} sample {:#x} -> {} member(s), gain {:.3}",
                voice.sample,
                members.len(),
                voice.gain
            );
        }
    }

    /// Dump every member of the three containers, to see what actually plays.
    ///
    ///     cargo test -p skate-game --bin skate3rust -- --ignored dump_session_marker_members --nocapture
    #[test]
    #[ignore = "needs the owner's assets"]
    fn dump_session_marker_members() {
        use skate_audio_core::authored::oneshot::Rand;
        use skate_data::audio::splice::{SpliceBanks, SpliceState};
        let assets =
            std::path::Path::new("C:/s3/installations/70eda9dc4644496d81ae73af95ff4285/assets");
        let vault = Collections::load(assets).expect("vault");
        let sounds = FrontEndSounds::load(&vault);
        let bank = skate_data::audio::catalog::MENU_BANK;
        let banks = SpliceBanks::load(
            &assets.join("private/stock/data/audio/audiofiles.big"),
            &[bank],
            &[bank],
        )
        .expect("bank");
        for sound in [FrontEndSound::PlaceMarker, FrontEndSound::GotoMarker] {
            let voice = sounds.voice(sound).expect("resolved");
            // Resolve several times: a Splice container can be a random one-of, in which case the
            // membership changes between passes and playing "all members" would be wrong.
            for pass in 0..4 {
                let mut state = SpliceState::default();
                let mut rand = Rand::new(pass + 1);
                let members = banks
                    .resolve(bank, voice.sample, &mut state, &mut rand)
                    .expect("resolve");
                let summary: Vec<String> = members
                    .iter()
                    .map(|m| {
                        format!(
                            "rec{} s{:#x} off{:#x} gain{:.2} pitch{:.2} delay{:.3} pan{:.0}",
                            m.record,
                            m.sample,
                            m.stream_offset,
                            m.values.gain,
                            m.values.pitch,
                            m.values.delay,
                            m.pan
                        )
                    })
                    .collect();
                println!("{sound:?} pass{pass}: {} member(s)", members.len());
                for line in summary {
                    println!("    {line}");
                }
            }
        }
    }
}
