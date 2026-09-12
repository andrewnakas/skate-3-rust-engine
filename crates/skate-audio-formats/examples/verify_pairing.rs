//! Cross-check a `.snr` header against its paired `.sns` block chain.
//!
//! The header claims a total sample count; the block chain carries per-block counts.
//! If the parsers are right, the blocks must sum to the header. This is the strongest
//! available correctness check short of decoding audio.
//!
//!     cargo run --example verify_pairing -- <resident.big> <payload.big>

use skate_audio_formats::{eaac, eb};
use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let resident = std::fs::read(args.next().ok_or("need <resident.big>")?)?;
    let payload = std::fs::read(args.next().ok_or("need <payload.big>")?)?;

    // Report header parse failures rather than dropping them: a silently missing header
    // shows up later as "unpaired", which looks like a data problem and is not.
    let mut headers: HashMap<String, eaac::SnrRecord> = HashMap::new();
    for e in &eb::Archive::parse(&resident)?.entries {
        let Some(name) = e.name.as_deref() else { continue };
        let Some(stem) = eaac::stem(name) else { continue };
        let Some(bytes) = resident.get(e.range()) else {
            println!("  {name:38.38} range outside archive");
            continue;
        };
        match eaac::SnrRecord::parse(bytes) {
            Ok(rec) => {
                headers.insert(stem.to_owned(), rec);
            }
            Err(err) => println!("  {name:38.38} header parse failed: {err}"),
        }
    }

    let mut ok = 0;
    let mut mismatched = 0;
    let mut unpaired = 0;
    for entry in &eb::Archive::parse(&payload)?.entries {
        let Some(name) = entry.name.as_deref() else { continue };
        if !name.ends_with(".sns") {
            continue;
        }
        let Some(stem) = eaac::stem(name) else { continue };
        let Some(rec) = headers.get(stem) else {
            println!("  {name:38.38} NO HEADER");
            unpaired += 1;
            continue;
        };
        let Some(bytes) = payload.get(entry.range()) else { continue };
        let blocks = eaac::blocks(bytes)?;
        let summed: u64 = blocks.iter().map(|b| u64::from(b.num_samples)).sum();
        let claimed = u64::from(rec.header.num_samples);
        if summed == claimed {
            ok += 1;
        } else {
            mismatched += 1;
            let delta = summed as i64 - claimed as i64;
            println!(
                "  {name:38.38} blocks={:<5} summed={summed} claimed={claimed} delta={delta:+}",
                blocks.len()
            );
        }
    }
    println!("\n  exact match: {ok}   mismatched: {mismatched}   unpaired: {unpaired}");
    Ok(())
}
