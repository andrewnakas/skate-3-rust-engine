//! Find which bank holds a sample the running game opened, from what the open probe logs:
//! the descriptor's slot index and the EAAC header's second word.
//!
//!     cargo run --release --example find_samples -- AUDIOFILES.BIG INDEX:WORD [INDEX:WORD ...]
//!
//! INDEX and WORD are hex, as the probe prints them (INDEX is the descriptor byte minus one).

use skate_audio_formats::{banks, eb};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: find_samples ARCHIVE INDEX:WORD...")?;
    let wanted: Vec<(usize, u32)> = args
        .map(|a| {
            let (i, w) = a.split_once(':').ok_or("INDEX:WORD")?;
            Ok((usize::from_str_radix(i, 16)?, u32::from_str_radix(w, 16)?))
        })
        .collect::<Result<_, Box<dyn std::error::Error>>>()?;
    let data = std::fs::read(&path)?;
    let archive = eb::Archive::parse(&data)?;
    for entry in &archive.entries {
        let Some(name) = entry.name.as_deref() else { continue };
        if !name.ends_with(".abk") || entry.is_compressed() {
            continue;
        }
        let bytes = &data[entry.range()];
        let Ok(abk) = banks::Abk::parse(bytes) else { continue };
        let mut hits = Vec::new();
        for &(index, word) in &wanted {
            let Some(range) = abk.sample_range(index) else { continue };
            if range.len() < 8 {
                continue;
            }
            let second = u32::from_be_bytes(bytes[range.start + 4..range.start + 8].try_into()?);
            if second == word {
                hits.push(format!("{index:x}:{word:x}"));
            }
        }
        if !hits.is_empty() {
            let exports: Vec<&str> = abk.exports.iter().map(|e| e.name.as_str()).collect();
            println!("{name}: {} of {} match [{}], exports {:?}", hits.len(), wanted.len(), hits.join(" "), exports);
        }
    }
    Ok(())
}
