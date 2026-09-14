# Player sounds in the Rust build: handoff (2026-09-14)

This is for a Claude session on Windows. The goal is to finish player-character sound in
`skate-3-rust-engine`. It covers where things stand, what to copy over, how to build and check the
work, and what is left.

**Scope, set by the user:** only the player character's sounds. That means footsteps, rolling,
grinds, slides, flips, wheel skids, foot drag and cloth foley. Other skaters, pedestrians
("livingworld"), ambience and music are out of scope.

## Short answer: is it done?

**No. Player sounds are not audible in the Rust build yet.** The chain from a game event to a
built voice graph exists in Rust and matches the game where it was checked. The last three
audio-thread stages are missing, so no frames come out, and the engine's gameplay does not post
sound events yet.

| stage | Rust | status |
|---|---|---|
| retail archives, banks, `.csi` symbols, EAAC headers | `skate-audio-formats` | validated on real data |
| XMA decode to PCM | `skate-data/src/audio` (the `ffmpeg` binary) | byte-exact |
| a stream played through Bevy | `skate-game/src/skate_audio.rs` (`SKATE_AUDIO_PLAY`) | confirmed at the sound card |
| bank install, symbol binding, message post, instance spawn | `skate-audio-core` `patch.rs`, `symbols.rs` | unverified new work; trace-confirmed binding |
| the patch evaluator and its opcodes | `eval/` | ported, most opcodes verified |
| the voice op (slot 27) and property routing | `voice.rs` | unverified |
| voice device open: class registration, graph build, sends, pan | `device.rs`, `classes.rs`, `modules.rs` | unverified; the module list matches the game |
| graph pass and every voice kernel | `graph.rs`, `kernels.rs`, `filters.rs`, `gains.rs`, `pitch.rs`, `mix.rs`, `bus.rs` | kernels verified; routing unverified |
| sound player render | `sndplayer.rs`, `stream.rs` | unverified |
| **command-ring drain** (`sub_82B48530`, phase 4) | — | **missing** |
| **play command** (`sub_82B32DC8`) | — | **missing** (C++ reference exists) |
| **stream pump** (`sub_82B31EE0`), where decoded PCM must enter | — | **missing** |
| per-block driver: drain, evaluator tick, graph passes, mix to output | — | **missing** |
| gameplay posting player events, with per-frame updates | — | **missing** |

## Evidence so far

Everything below uses `crates/skate-audio-core/examples/grind_instance.rs` against real data.

- **Footstep, against a real game trace.** The program is `playercharacter_footstep` in
  `fstep_skateshoe1_sm.abk`. It was driven by the traced post payload and its 409 traced update
  payloads, one update every 6 audio frames.
  - It opens eight layers on the same update as the game, the one that sets payload word 8 to 1.
  - The layers come from the same sample groups.
  - Voice properties 3, 5, 6, 7, 8 and 9 are identical to the game's.
  - Variant, pitch and two start gains differ. That is consistent with the random counter at
    `0x830775F0` starting from boot state, but not proved.
- **Voice graphs.** With `GRAPH=1` the Rust device open builds, for every footstep layer, the module
  list the game logged: `SndPlayer1, Rechannel, Resample, HighPassIir2, LowPassIir2, Send, Gain` at
  one channel, then `Pan2D1` and `Send` at six.
- Tests: 678 in `skate-audio-core`, 33 in `skate-audio-formats`, all passing on Linux.

Full write-up: sk8Audio `docs/audio-banks.md`, sections "The footstep program against the game" and
"From a message to the evaluator". The plan is sk8Audio `docs/PLAN.md`, Phase 6.

## What to bring to Windows

Neither repo is pushed. On 2026-09-14:
- this repo's `main` is 58 commits ahead of `origin/main`;
- sk8Audio's `phase0-measurements` is 84 ahead of its origin.

Push them, or copy them, before switching.

1. **This repo** (`skate-3-rust-engine`). The audio crates are vendored under `crates/`.
2. **sk8Audio** (`github.com/andrewnakas/skate3-audio`, branch `phase0-measurements`). It is the
   source of truth for the audio crates, and holds the C++ references the remaining ports are
   transcribed from. Develop in `sk8Audio/rust/*`, then copy into `crates/` here.
   `tools/sync_rust_engine.sh push` does that on Linux; on Windows run it in Git Bash with
   `SKATE_RUST_ENGINE` set, or copy the two crate folders by hand, excluding `target/` and
   `Cargo.lock`.
3. **Game audio archives**, from the user's own disc, never committed:
   - `audiofiles.big` (every player bank), `wheels.big`, `grains.big`;
   - `ambience.big` and `ambienceresident.big`, only for the existing stream check.

   On Linux they are in `~/Documents/skate3/Skate3Recomp-Linux/game/data/audio/`. On Windows they
   are in the extracted game's `data/audio` folder.
4. **The decrypted guest image**, `g_8200.bin` … `g_8320.bin`, 21 MB, never committed. The Rust
   runtime reads guest rodata from it: class descriptors, rodata constants, the trig table, the
   object table. `default.xex` has compressed `.rdata`, so this cannot be read off disk. It was
   dumped from the Linux recomp under gdb (sk8Audio `probe/harness/dump_image.gdb`). The copy is
   in sk8Audio `probe/harness/out/image/`.
5. **Traces.** `probe/traces/fstep_updates_40C02FD0.txt` (in sk8Audio git) holds the 409 footstep
   updates. `probe/harness/out/msgs1.log` (2.1 MB, untracked) is the played session: every post,
   update, voice open and graph build.
6. **ffmpeg.** `ffmpeg.exe` on `PATH`, or set `SKATE_FFMPEG` to its full path.

## Building on Windows

Per this repo's README: Rust with the MSVC toolchain, LLVM in its default location, then
`BUILD.bat` and `PLAY.bat`.

For the audio crates alone:

```powershell
cargo test --release -p skate-audio-formats
cargo test --release -p skate-audio-core
```

Every guest address in them is a constant, and they never touch host memory layout, so they
should behave the same on x86-64 Windows. Several ports are `#[cfg(target_arch = "x86_64")]`,
which is fine. **Run the tests first and report any difference from Linux before building on
them.**

**Windows path trap.** `SKATE_AUDIO_PLAY=<archive>:<entry>:<channels>:<rate>[:<blocks>]` splits on
`:` from the right. A drive letter (`C:\...`) breaks the four-field form, so always pass all five
fields. For example `C:\game\data\audio\ambience.big:0:5:48000:0`, where `0` blocks means all.

### Reproducing the footstep result

```powershell
$env:PAYLOAD = "00000000 00000000 00001000 00000000 000061A8 00000000 00000000 00007FFF 00000000 00000000 00000000 00000000 00000001 00000001 00000000 00000000 00000001 00000001 00000000 00000000 00000000 00000000 00000000 00000000 0000000C"
$env:UPDATE_FILE = "<sk8Audio>\probe\traces\fstep_updates_40C02FD0.txt"
$env:UPDATE_EVERY = "6"; $env:FRAMES = "70"; $env:GRAPH = "1"
cargo run --release -p skate-audio-core --example grind_instance -- <audio>\audiofiles.big <image dir> fstep_skateshoe1_sm.abk playercharacter_footstep
```

What to expect at frame 59:
- seven `open voice` lines;
- one more at frame 65;
- each followed by `graph for voice …: 9 modules [SndPlayer1x1 Rechannelx1 Resamplex1 HighPassIir2x1 LowPassIir2x1 Sendx1 Gainx1 Pan2D1x6 Sendx6]`.

Other examples: `voice_classes <image dir>` prints the cooked class defaults. In
`skate-audio-formats`, `find_samples` matches a traced open to its bank, and `bind_banks` (in
core) resolves every bank export.

## How a player sound flows, with addresses

1. **Gameplay** calls a per-object message constructor; the handle slot for object *i* is
   `0x8302EE28 + 8i`:

   | object | constructor | layout known |
   |---|---|---|
   | grind | `sub_824AF8C8` | yes, `docs/audio-banks.md` |
   | footstep | `sub_824B73E0` | yes, 25 words, from the trace |
   | rolling | `sub_824C4C18` | read from the trace only |
   | board slide | `sub_824B0670` | no |
   | body slide | `sub_824B7070` | no |
   | flips | `sub_824AFAD8` | read from the trace only |
   | wheel skid | `sub_824AF678` | read from the trace only |
   | foot drag | `sub_824AF498` | read from the trace only |

   Messages are held: the game re-delivers updated payloads every frame
   (`sub_828E2D18`, `patch::redeliver`; the grind updater is `sub_824C39E0`). The object table is at
   `0x8302D4A4` (72 entries).
2. **Post** `sub_828E2B48` (`patch::post`) → the bank listener `sub_82B1DAD0` → **instance**
   `sub_82B1D880`, linked onto the evaluator list at `0x83036F4C`.
3. **Evaluator** `sub_82B1E290` (`interp::tick_with`), once per 256-frame audio block at 48 kHz
   (187.5/s). Game init sets its period denominator `0x82FD35F4` to 30.0.
4. **Voice op** (slot 27, `sub_82B1D240`, `voice::voice_op`) → the device's open.
5. **Device open** `sub_824A3140` (`device::open_voice_graph`):
   - it registers the classes (`classes.rs`);
   - it builds the graph (`modules::build_graph`);
   - it enqueues commands on the system ring (`system+48`, write offset `+204`):
     - install `0x82B49210`;
     - player gain `0x82B49268`;
     - play `0x82B32DC8`;
     - send bus `0x82B31680`;
     - property stamp `0x82B463A8`.
6. **Drain** (missing). Phase 4 of `sub_82B48530` walks the ring, calls each record's handler,
   and advances by the size it returns. It then raises the high-water mark (`+208`), zeroes
   `+204` and counts drains at `+256`. Handlers already ported:
   - `modules::install_command`, which inserts the player into `system+108`;
   - `leaves::publish_float`;
   - `leaves::stamp_slot`;
   - `voices::repoint_link` (`0x82B31680`).
7. **Play** (missing). Transcribe `sub_82B32DC8` from sk8Audio
   `recomp/src/audio_ports/sub_82B32DC8.inc`:
   - claim the SndPlayer1 ring slot at write index `+467`;
   - run `bitstream::parse_voice_header` and `decode_packet_header`, both ported;
   - advance the index.

   The record kind `+73` is bits 31-30 of the sample header's second word. A bank sample that does
   not loop (footsteps) is kind 0 and takes the short path. Looping or streamed samples (grinds
   loop) take the long path, which needs:
   - `sub_82B47A68`, the voice acquire (C++ `.inc` exists);
   - `sub_82EC0E08`, the callback scheduler (no port);
   - a name allocation.
8. **Stream pump** (missing), the node callback `sub_82B31EE0` (gate 1: it submits to the XMA
   decoder under critical sections). It moves a record to state 2 or 3 and readies the consumer slot
   (`object + 16*[object+474]`, byte `+113` = 1). That is what `sndplayer::render_block` waits for.
   **Engine-decoded PCM enters here.** Replace the XMA submit with `skate-data` decode. Then
   `stream_remaining` and `deliver_frames` (with a `StreamFill`) must see the frames the pump
   would have produced.
9. **Graph pass** per block, for each installed player: `graph::run_pass` with
   `kernels::VoiceKernels` (process = SndPlayer1 → … → Send → `bus::mix_source`). Then mix the buses
   to output. The 6-channel panner means a 5.1 bus, so the engine must downmix for Bevy. The
   top-level per-block function on `RwAudioCore Dac`, which calls the drain and the passes, is **not
   yet mapped for Rust**. Start from `sub_82B48530`'s caller.

## Work order

1. **Drain.** A Rust dispatcher by handler address over the ported handlers; an unknown handler
   is an `Err` naming it. Locking and the `mftb` timing stores are not reproduced; say so in the
   module note.
2. **Play command**, short path from the `.inc`; the long path behind a host trait.
3. **Stream pump replacement.** Read `sub_82B31EE0.inc` and `sndplayer.rs` first. Design the
   engine-PCM `StreamFill`, and write down exactly which fields it sets and why.
4. **Per-block driver.** Evaluator tick, drain, graph passes, then output into
   `skate_audio.rs`'s Bevy source. Get one footstep audible, driven by the example's traced
   payloads.
5. **Voice lifetime.** The voice vtable `+0` release `0x82B1E458` has a C++ `.inc`. Also SndPlayer1
   configure ids 1-4 (only 0 and 5 are ported) and bus creation `sub_824916E8`, which is a host stub
   today.
6. **Gameplay events.** Post and update each player object from the engine's skating state. The
   payload layouts are in the constructors above; read each one, and check against `msgs1.log`.
7. **Grinds and other looping sounds** need the play command's long path (7 above).
8. **Guest image at runtime.** Development can load the dumped `g_*.bin`. A release must build
   those regions from the user's own `default.xex` during setup (XEX2 decrypt and decompress). Not
   started.

## Rules carried over from sk8Audio (read its `CLAUDE.md`)

- **Exact means exact.**
  - Transcribe store order, reloads, 64-bit adds and `fctiwz`/`fctidz` edges.
  - Scalar `fmadds`/`fmsubs` are fused; vector multiply-adds round twice.
  - Compute `lis` constants as `((hi & 0xFFFF) << 16) + lo`; never read them by eye.
- **Label unverified work** in module notes and the crate README table, and keep the README test
  count current.
- **Real data beats unit tests.** Check new stages against `msgs1.log` and the retail archives.
- **Never commit or distribute game assets**: audio archives, the image dump, the soundtrack.
- Commit messages end with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.

## A prompt to start the Windows session

> Read `docs/audio-player-sounds-handoff.md` in skate-3-rust-engine, then sk8Audio's `CLAUDE.md`
> and `docs/PLAN.md` Phase 6. First run the two audio crates' tests and the footstep reproduction
> on Windows, and report any difference from Linux. Then work the "Work order" list from item 1:
> drain, play command, stream pump replacement, per-block driver. The first milestone is one
> footstep audible in `skate3rust.exe`. Only the player character's sounds are in scope.
