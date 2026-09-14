//! Run a real player sound's patch as far as the Rust runtime reaches: install the shipped projects
//! and `GRINDS.abk`, post a `Class_grind` message the way the game does, and tick the evaluator.
//!
//! Guest memory is the dumped image (`probe/harness/out/image/g_*.bin`), so every constant cell an
//! opcode reads holds its real value; the runtime's own globals are zeroed first, since the dump was
//! taken at boot. The run stops at the first opcode that is not ported and names it, which is the
//! point: it measures how much of a real grind program the ported table covers.
//!
//!     cargo run --release --example grind_instance -- <audiofiles.big> <image dir> [bank] [object]

use skate_audio_core::eval::interp;
use skate_audio_core::patch::{self, BumpHeap};
use skate_audio_core::{Guest, Segment, symbols};
use skate_audio_formats::eb;

const MISC: u32 = 0x4000_0000;
const ARENA: u32 = 0x5000_0000;
const HEAP: u32 = 0x6000_0000;
const HEAP_BYTES: u32 = 0x40_0000;
const STACK: u32 = 0x7000_0000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let archive_path = args.next().ok_or("usage: grind_instance <audiofiles.big> <image dir> [bank] [object]")?;
    let image = args.next().ok_or("need the image dump directory")?;
    let bank_name = args.next().unwrap_or_else(|| "GRINDS.abk".into());
    let object = args.next().unwrap_or_else(|| "Class_grind".into());

    let mut segments = Vec::new();
    for entry in std::fs::read_dir(&image)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some(hex) = stem.strip_prefix("g_") else { continue };
        segments.push(Segment { base: u32::from_str_radix(hex, 16)? << 16, bytes: std::fs::read(&path)? });
    }
    let mut g = Guest::from_segments(segments);
    g.put(MISC, vec![0; 0x1000]);
    g.put(HEAP, vec![0; HEAP_BYTES as usize]);
    g.put(STACK - 0x1000, vec![0; 0x2000]);
    for cell in [symbols::PROJECT_LIST_HEAD, interp::LIST_HEAD, patch::BANK_LIST, interp::DELTA_CACHE,
                 interp::FRAME_COUNT, interp::COUNTDOWN, interp::SCALE_GLOBAL] {
        g.set_u32(cell, 0)?;
    }
    g.set_u16(symbols::GENERATION, 0)?;
    // Game init (sub_826D4C30) sets the evaluator's period denominator to 30.0 after the dump was
    // taken, so a period is 1/30 s in play, not the dump's 1/41.6.
    g.set_u32(interp::PERIOD_DENOM, 30.0f32.to_bits())?;

    let data = std::fs::read(&archive_path)?;
    let archive = eb::Archive::parse(&data)?;
    let mut next = ARENA;
    let mut place = |g: &mut Guest, bytes: &[u8]| {
        let at = next;
        g.put(at, bytes.to_vec());
        next = (at + bytes.len() as u32 + 0x1000) & !0xFFF;
        at
    };
    // Every project, so the object resolves through the same two passes as in the game.
    let mut game_project = None;
    for e in &archive.entries {
        let Some(name) = e.name.as_deref() else { continue };
        if name.ends_with(".csi") {
            let bytes = &data[e.range()];
            let csi = skate_audio_formats::banks::Csi::parse(bytes)?;
            if let Some(s) = csi.symbols.iter().find(|s| s.group == 1 && s.name == object) {
                game_project = Some((csi.project_id, s.id));
            }
            let at = place(&mut g, bytes);
            symbols::install_project(&mut g, at)?;
        }
    }
    let (project, name_id) = game_project.ok_or("no shipped project names the object")?;
    let member = archive
        .entries
        .iter()
        .find(|e| e.name.as_deref() == Some(bank_name.as_str()))
        .ok_or("bank not in the archive")?;
    let bank = place(&mut g, &data[member.range()]);
    let (id, first) = patch::load_bank(&mut g, bank, STACK)?;
    println!("{bank_name} installed at {bank:#x}: id {id}, first bank {first}");

    // The game's side: a slot resolved from its object table entry, then a post.
    let (slot, query, message) = (MISC + 0x10, MISC + 0x20, MISC + 0x100);
    g.set_span(MISC + 0x40, object.as_bytes())?;
    g.set_u8(MISC + 0x40 + object.len() as u32, 0)?;
    g.set_u32(query, MISC + 0x40)?;
    g.set_u16(query + 4, project)?;
    g.set_u16(query + 6, name_id)?;
    let found = symbols::lookup_table1(&mut g, slot, query)?;
    println!("{object} {project:#06x}:{name_id:#06x} resolved: {found:?}");

    // Class_grind's payload as sub_824AF8C8 builds it: fixed header words, then a speed of 5000,
    // 1024, surface class 3, variant 0, level 20000, three flags off, and two zero words.
    let payload: [u32; 17] = [0, 32767, 0, 0, 0, 25000, 0, 5000, 1024, 3, 0, 20000, 0, 0, 0, 0, 0];
    for (i, w) in payload.iter().enumerate() {
        g.set_u32(message + 4 + 4 * i as u32, *w)?;
    }
    let mut heap = BumpHeap { next: HEAP, end: HEAP + HEAP_BYTES };
    let status = patch::post(&mut g, &mut heap, slot, message + 4, message)?;
    let head = g.u32(interp::LIST_HEAD)?;
    println!("post: {status}; interpreter list head {head:#x}");
    if head == 0 {
        return Ok(());
    }
    let instance = head - 8;
    let program = g.u32(instance + 16)?;
    let block = g.u32(instance + 20)?;
    let first_words: Vec<String> = (0..12).map(|i| format!("{:08x}", g.u32(block + 4 * i).unwrap())).collect();
    println!("instance {instance:#x}: program {program:#x}, block {block:#x}, block words {}", first_words.join(" "));

    // 256 samples at 48 kHz per audio frame.
    let delta = (256.0f32 / 48_000.0) as f64;
    let mut ops = 0usize;
    for frame in 0..2000 {
        match interp::tick_with(&mut g, delta, &mut patch::PatchHost { heap: &mut heap }) {
            Ok(t) => {
                ops += t.ops;
                if frame < 12 && t.walked {
                    println!("  frame {frame}: walked {} nodes, {} ops", t.nodes, t.ops);
                }
                if t.walked && g.u32(interp::LIST_HEAD)? == 0 {
                    println!("  frame {frame}: the list emptied");
                    break;
                }
            }
            Err(e) => {
                println!("  frame {frame}: stopped after {ops} ops: {} (at {:#x})", e.message, e.address);
                let record = g.u32(instance + 16)?;
                println!("  period {} frames, scale {}", g.u32(interp::FRAME_COUNT)?, g.f32(interp::SCALE_GLOBAL)?);
                let _ = record;
                break;
            }
        }
    }
    Ok(())
}
