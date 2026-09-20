# Engine defects found while porting player audio (as of 2026-09-20)

These are **engine/physics/gameplay** defects, not audio-port gaps. Porting the audio exactly made
them visible, because retail's audio reads native physics fields directly and therefore only sounds
right when those fields carry retail's values.

Each entry says how it was found, how confident the diagnosis is, and what would prove it fixed.
Confidence is deliberately separated from severity: some of these are measured, others were seen
once in a playtest and have no repro yet.

---

## 1. Jump height and the landing's weight — **resolved by measurement; no workaround left**

**History, because two wrong turns are recorded in the git log.** An earlier session measured
`jump_height_200` at 0.10–0.13 m against a retail capture's 1.1–2.2 m and concluded the field was
~10x short, so a 10x was applied to the copy handed to audio. On 2026-09-20 that was moved onto the
real launch (`ground_jump`'s `remaining` term) on the theory that the jump itself was short; the
owner playtested it and went into orbit, so the launch was ruled out and the change reverted.
`ground_jump` is untouched and retail-exact.

**What the trace actually shows.** Measuring Class_Treatment word 9 directly from a playtest
(`SKATE_AUDIO_TRACE`, 27541 updates, 1559 with a live landing):

| | word 9 (`trunc(clamp(h × 166.667, 0, 1000))`) |
|---|---|
| Retail | 183–367 |
| This engine **with** the 10x | min 62, **p50 1000, p90 1000** — pegged at the clamp |

So the 10x drove word 9 3–5x past retail and into saturation: every landing played at maximum
weight regardless of the hop, which is what the owner heard as "landings too loud for the height".
Back-solving the clamp puts the *reported* height at **≥0.6 m**, not the 0.10–0.13 m the original
measurement claimed — so the field is far less wrong than believed, and word 9 without any scale
falls around 100–300, inside retail's range.

**Resolution.** The scale is removed entirely. No workaround remains in the tree.

**Residual, low priority.** Reported height (~0.6 m) still sits below retail's 1.1–2.2 m plateau,
so word 9 may run slightly low on big drops. If that ever matters, the thing to check is that
`max_y` tracks the deck rigid body (`air_phase.rs:202-206`, `part_transforms()[6]`) while `start_y`
is a wheel-centre ground reference — a constant offset between two different references is
subtracted out of every hop. PhysicsAir and KnownAir also disagree on that reference: `+500`
(current, `air_phase/input.rs:49`) versus `+480` (previous, `known_air.rs:94`). **Do not "fix" this
at the launch** — that was tried and it sends the skater into orbit.

## 2. The engine rarely enters KnownAir — **diagnosed from code + capture**

**Symptom.** Originally: no landing impact at all. Every per-wheel landing bucket stayed 0.

**Evidence.** Retail is in KnownAir (state 201) for essentially every hop. This engine selects it
only while Processed2468 bit 10 (trajectory valid) is published, and nothing publishes that bit
except transiently, so ordinary ollies run in PhysicsAir (200/202). PhysicsAir publishes nothing
for Air+176 and a constant 4.0 for Air+184, so audio state `+236` stayed 0 and with it `+244` →
`+448`. Documented at `crates/skate-game/src/physics/audio_observation.rs:182-207`.

**Current workaround.** Audio reads the owning state's own timer/prediction instead
(`audio_observation::air_timing`). This restored landing impacts, but it is audio-side
compensation for a physics-side defect — anything else that reads air timing still gets the wrong
values.

**Fixed when.** An ordinary ollie runs in state 201, and `air_timing` can be deleted in favour of
reading Air+176/+184 directly.

## 3. Powerslide squeaks never fire — **reproduced, root cause not isolated**

**Symptom.** No powerslide squeak sound, ever.

**Evidence.** Retail's gate is: both feet inside the deck box (`+615`/`+616`), more than one wheel
down, and `|deck tilt +264| × 114.5916 ≥ 15` (retail's slides sit at 0.14–0.28 rad). In a Rust
playtest the squeak never posts, so at least one of those inputs does not reach retail's values
during PhysicsSlideGround (state 101). Which one is not yet isolated.

**How to isolate.** Run with `SKATE_AUDIO_TRACE=<file>`, powerslide, and compare the `AS` lines
(they carry `tilt264`, `feet615`, `feet616`, `wheels200`) against the retail capture's `state.tsv`
for the same manoeuvre. The mismatching field is the defect.

## 4. Bail / ragdoll native outputs are absent — **known gap**

The bail audio families (`c_cloth_falls`, `c_body_slide`, `c_board_slide`) are gated on engine
outputs that do not exist: ragdoll body-part slide contacts, thrown/loose-board contact, and the
bail flags and speeds (`+676`/`+677`, `+328`/`+672`). Per the project rule these families stay off
and are reported rather than approximated, so **a bail is currently silent** beyond what other
families happen to emit.

**Fixed when.** The engine publishes per-body-part ragdoll contacts and loose-board contact, at
which point the three families can be driven from real inputs.

## 5. Surfaces are not implemented — **known, deliberate**

Everything runs on default surface 2 (concrete_rough). Retail routes rolling grains, seam patterns,
wheel-pop sample categories, grind timbre and skid sounds per surface. Until surfaces exist, all of
those are correct-but-monotonous: the right sound for concrete, everywhere, including on wood,
metal and grass.

**Fixed when.** The engine publishes real per-wheel audio materials and the map's seam pattern
(`surface_tag >> 12 & 0xF`), so the already-ported per-surface tables get exercised.

## 6. Physics NaN torque crash on landing — **seen once, no repro**

A playtest crashed on a landing with a NaN torque in the physics solver. It is unrelated to audio
and has not been reproduced since; no current log carries it. Listed so it is not forgotten, but it
needs a repro before it can be chased. Treat the diagnosis as unconfirmed.

## 7. Broadphase disagrees with a full scan — **reproducible test failure**

`physics::board_world::broadphase_tests::predictive_contacts_and_retention_match_full_scan_for_every_primitive`
(`crates/skate-core/src/physics/board_world/tests.rs:232`) fails deterministically:
`world.query_primitives(..)` returns a different contact set from the linear reference scan
`linear.query_primitives(..)` for some primitive/capacity/`deferred_reduction` combination.

Repro: `cargo test -p skate-core --lib --locked predictive_contacts_and_retention`

Confirmed pre-existing as of commit `330a3e9` — it is not a consequence of the pop-height change.
It matters because the broadphase is what feeds collision to the whole board simulation, so a
disagreement with the reference scan is a correctness bug in contact generation, not just a
failing test. The assertion dump is large; the first divergence is in the retention records, so
start by narrowing which `capacity` / `deferred_reduction` pair diverges.

---

## Suggested order for the next session

1. **#2 (KnownAir)** — unblocks real air timing for everything, not just audio, and lets an
   audio-side workaround be deleted. (#1 is resolved; nothing to do there.)
3. **#7 (broadphase)** — a reproducible correctness failure in contact generation, and it has a
   one-line repro.
4. **#3 (powerslide inputs)** — one trace comparison away from being isolated.
5. **#5 (surfaces)** — large, but the audio side is already ported and waiting.
6. **#4 (ragdoll/loose board)** and **#6 (NaN crash, once reproduced)**.
