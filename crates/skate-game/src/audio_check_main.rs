//! Play one of the game's audio streams through the engine's real audio path, and exit.
//!
//! The plugin that plays game audio lives inside a binary that needs a set-up asset pipeline to
//! boot, which makes the audio path awkward to exercise on a development checkout. This runs the
//! same plugin, the same Bevy audio source and the same mixer, with nothing else: no window, no
//! renderer, no assets. It is how the claim "this plays" gets checked rather than asserted.
//!
//!   SKATE_AUDIO_PLAY=<archive>:<entry>:<channels>:<rate>[:<blocks>] \
//!     skate3-audio-check [seconds]
//!
//! It exits when the stream has finished, or after `seconds` (default 20) so it can never hang a
//! script. Recording the output device's monitor while this runs is what turns "the sink reports
//! playing" into evidence that samples reached the device.

mod skate_audio;

use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::asset::AssetPlugin;
use bevy::audio::{AudioPlugin, AudioSink};
use bevy::log::LogPlugin;
use bevy::prelude::*;

#[derive(Resource)]
struct Deadline {
    started: Instant,
    limit: Duration,
    /// Set once a sink has existed, so "the sink went away" means finished rather than not yet.
    played: bool,
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20);
    if std::env::var("SKATE_AUDIO_PLAY").is_err() {
        eprintln!("set SKATE_AUDIO_PLAY=<archive>:<entry>:<channels>:<rate>[:<blocks>]");
        std::process::exit(2);
    }
    if !skate_audio::decoder_available() {
        eprintln!("no decoder on PATH; set SKATE_FFMPEG");
        std::process::exit(2);
    }

    App::new()
        // A fixed tick keeps this from spinning a core while the mixer does the real work.
        .add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_millis(16))))
        .add_plugins((LogPlugin::default(), AssetPlugin::default(), AudioPlugin::default()))
        .add_plugins(skate_audio::SkateAudioPlugin)
        .insert_resource(Deadline {
            started: Instant::now(),
            limit: Duration::from_secs(seconds),
            played: false,
        })
        .add_systems(Update, finish)
        .run();
}

/// Exit when the stream has played out, or when the deadline passes.
fn finish(
    mut deadline: ResMut<Deadline>,
    sinks: Query<&AudioSink>,
    mut exit: MessageWriter<AppExit>,
) {
    let live = sinks.iter().count();
    if live > 0 {
        deadline.played = true;
    } else if deadline.played {
        info!("skate-audio-check: the stream finished after {:.1?}", deadline.started.elapsed());
        exit.write(AppExit::Success);
        return;
    }
    if deadline.started.elapsed() >= deadline.limit {
        if deadline.played {
            info!("skate-audio-check: still playing at the deadline; stopping");
        } else {
            warn!("skate-audio-check: nothing ever played within {:?}", deadline.limit);
        }
        exit.write(if deadline.played { AppExit::Success } else { AppExit::error() });
    }
}
