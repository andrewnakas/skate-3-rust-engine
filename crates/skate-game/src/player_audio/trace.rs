//! Playtest packet trace in the retail recomp capture's own line format, so
//! `tools/audio_capture_extract.py` splits a Rust playtest exactly like the retail capture and the
//! two can be compared word for word.
//!
//! Enabled by `SKATE_AUDIO_TRACE=<file>`. Lines (`<ms> <frame> <TAG> …`, hex words):
//!
//! * `PO <slot> <index> <name> <payload> 0 | 28 words | handle=<handle>` — a post;
//! * `UP <node> <name> <payload> | 28 words` — a redelivery;
//! * `RL <node> <name>` — a release;
//! * `AS <field>=<value> …` — the audio-state fields the families' triggers read.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

use super::audio_state::AudioState;

struct Trace {
    out: std::io::BufWriter<std::fs::File>,
    start: Instant,
    frame: u64,
    names: HashMap<u32, String>,
}

static TRACE: Mutex<Option<Trace>> = Mutex::new(None);
static CHECKED: std::sync::Once = std::sync::Once::new();

fn with<F: FnOnce(&mut Trace)>(f: F) {
    CHECKED.call_once(|| {
        if let Some(path) = std::env::var_os("SKATE_AUDIO_TRACE") {
            if let Ok(file) = std::fs::File::create(&path) {
                *TRACE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Trace {
                    out: std::io::BufWriter::with_capacity(1 << 20, file),
                    start: Instant::now(),
                    frame: 0,
                    names: HashMap::new(),
                });
            }
        }
    });
    if let Some(trace) = TRACE.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        f(trace);
    }
}

fn words(words: &[u32]) -> String {
    (0..28)
        .map(|i| format!("{:08X}", words.get(i).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Trace {
    fn line(&mut self, body: std::fmt::Arguments) {
        let ms = self.start.elapsed().as_millis();
        let _ = writeln!(self.out, "{ms} {} {body}", self.frame);
    }
}

pub(crate) fn frame(frame: u64) {
    with(|t| {
        t.frame = frame;
        if frame % 60 == 0 {
            let _ = t.out.flush();
        }
    });
}

pub(crate) fn post(object: &str, handle: u32, payload: &[u32]) {
    with(|t| {
        t.names.insert(handle, object.to_owned());
        t.line(format_args!(
            "PO 00000000 0 {object} {handle:08X} 0 | {} | handle={handle:08X}",
            words(payload)
        ));
    });
}

pub(crate) fn update(handle: u32, payload: &[u32]) {
    with(|t| {
        let name = t.names.get(&handle).cloned().unwrap_or_else(|| "(unknown)".into());
        t.line(format_args!("UP {handle:08X} {name} {handle:08X} | {}", words(payload)));
    });
}

pub(crate) fn release(handle: u32) {
    with(|t| {
        let name = t.names.remove(&handle).unwrap_or_else(|| "(unknown)".into());
        t.line(format_args!("RL {handle:08X} {name}"));
    });
}

pub(crate) fn state(a: &AudioState) {
    with(|t| {
        t.line(format_args!(
            "AS v208={:.3} wheels200={} tilt264={:.4} air332={} grind341={} feet615={} feet616={} slip232={:.4} trick348={} bail676={} walk716={} down724={} down725={} land448={:?} landed464={:?}",
            a.ground_speed_208,
            a.wheel_count_200,
            a.deck_tilt_264,
            u8::from(a.in_known_air_332),
            u8::from(a.grinding_341),
            u8::from(a.foot_in_deck_box_615),
            u8::from(a.foot_in_deck_box_616),
            a.slip_232,
            a.audio_trick_348 as i32,
            u8::from(a.bail_676),
            u8::from(a.walking_716),
            u8::from(a.foot_down_right_724),
            u8::from(a.foot_down_left_725),
            a.wheel_landing_bucket_448,
            a.wheel_landed_464,
        ));
    });
}
