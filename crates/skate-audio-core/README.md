# skate-audio-core

The Skate 3 audio graph in Rust, translated from native C++ that was **first proved equal to
the original recompiled code**. Every function here is a transcription of a body the shadow
harness compared against the running game, so a divergence points at the translation rather
than at a misunderstanding of the engine.

## What is here

| module | what it covers | verification |
|---|---|---|
| `system.rs` | the command ring producer, all four paths | 6,706 recorded comparisons replayed |
| `player.rs` | the three `PacketPlayer` consumers, the FIFO, the liveness scan | 1,679 replayed |
| `buffers.rs` | buffer-pair init | 222 replayed |

`cargo test` runs 13 unit tests pinning the record layouts, the FIFO state machine, the
liveness outcomes and the `fctidz` conversion. **Tier 1 is met: 8,607 of 8,607 comparisons
from one complete game session replay with 0 disagreements.**

```
cargo run --example replay_vectors -- VECTORS.tsv 00000000 3F800000
```

The vectors are recorded by the harness itself (`skate3_audio_vectors_path`) — real inputs the
game generated, not synthetic ones. An address the port reaches that was not recorded makes the
vector **unreplayable**, never zero-filled, because feeding it fabricated input would turn a
failure into a meaningless pass.

## Read this before trusting the green

That figure is real because four negative controls fail correctly — and because the *first*
control did **not** fail, which is how a vacuous-pass problem was found at all. Its limits are
in `docs/shadow-harness.md`:

- A large share of `EVENT_SUBMIT` passes verify one write **vacuously**: on an empty FIFO that
  word is already 0 on entry and 0 in the expectation.
- All recorded liveness queries arrived with an empty FIFO, so the list-walk branch is
  **unreachable by these sessions**, recorded as a permanent limit rather than a pending task.
- `EVENT_PLAY` is verified at one input point: 48 kHz, six channels.

Two divergences from the original are deliberate and pinned by tests. The guest's `fctidz` and
Rust's saturating `as i64` disagree at exactly 2^63, so the conversion is written branch for
branch; and the ring's publish ordering is inverted, which leaves memory byte-identical and is
therefore invisible to a single-threaded compare in either direction.

## Design

Guest structures are **byte-addressed big-endian accessors over `&mut [u8]`**, not idiomatic
Rust structs. These are recovered layouts with asserted offsets, and the per-function criterion
compares bytes against the verified C++ — a byte-addressed view makes that direct instead of
routing it through a serialisation step that could hide a discrepancy of its own. Offsets are
asserted at compile time, mirroring `docs/rw_audio_structs_check.c`.

Two callees are **closure parameters**, not ports: the decoder teardown and the restart branch,
both of which make indirect calls. Neither is portable, and the first is exactly why
`EVENT_STOP` has no comparable path under the harness.

## Not written yet

`scheduler.rs` (two-bucket tick), `xma.rs` (the paired ring protocol against a decoder trait),
`dsp/` (one file per plug-in family) and `graph.rs` (instantiation from descriptor metadata).
All 216 audio-thread functions now have a verified or gate-labelled C++ reference in
`recomp/src/audio_ports/`, so these are transcription work rather than analysis. See
`docs/PLAN.md` Phase 4 and section 6.
