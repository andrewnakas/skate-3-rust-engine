# Engine defects found while porting player audio (as of 2026-09-20)

These are **engine/physics/gameplay** defects, not audio-port gaps. Porting the audio exactly made
them visible, because retail's audio reads native physics fields directly and therefore only sounds
right when those fields carry retail's values.

Each entry says how it was found, how confident the diagnosis is, and what would prove it fixed.
Confidence is deliberately separated from severity: some of these are measured, others were seen
once in a playtest and have no repro yet.

---

## 1. Ollie / pop height is ~10x too small — **measured**

**Symptom.** Hops barely leave the ground compared with retail, and every landing sounds like the
lightest possible touchdown regardless of the trick.

**Evidence.** Audio state `+260` is Air+200 = `max_y − start_y` (deck part Y minus Processed+500),
written by KnownAir Fill `sub_82D36880` / `sub_82D34E90`. Measured per hop:

| | jump height | Treatment word 9 = `trunc(clamp(h × 166.667, 0, 1000))` |
|---|---|---|
| Retail capture | 1.1–2.2 m | 183–367 |
| This engine | 0.10–0.13 m | 16–22 |

So the engine sits in the bottom 2% of the range retail uses. The ratio is consistently ~10x, which
smells like a units or scale mismatch (m vs decimetres, or a settings value read at the wrong
scale) rather than a tuning difference.

**Suspected root cause (2026-09-20).** `ground_jump::calculate`
(`crates/skate-core/src/air/ground_jump/mod.rs`, lifted `Toolkit_CalcGroundJump` 0x82D93618)
launches with `remaining = max(height − current_height, 0)`, and since flight is purely ballistic
the apex rise *is* `remaining`. `current_height` is
`dot(animation_com_position − ground_reference_position, reference_up)`, wired to
`reckoning.vector_64` = the skater's full weighted body COM (`render_pose.rs:61-65`) minus the
lowest wheel centre (`player_input/ground_position.rs:16-41`) — i.e. an **absolute** ~0.85–0.95 m
standing height. With a ~1.0 m `JumpMaxHeight`, `1.0 − 0.9 = 0.1`: exactly what we measure.

Retail's Processed+592 is very likely the **crouch-relative** COM displacement (near zero at
launch), not the absolute COM height. Check whether `+592` should come from the animation
target/COM frame rather than `board_frames.centre_of_mass`, or whether `ground_reference_position`
should be the COM-plane reference instead of the wheel-centre ground point.

Two secondary candidates, in order: (a) the `vertical_response` graph is the only 16-point graph
read with **no 4-word header** while its siblings in the same collection use one
(`ground_animation/settings.rs:33-36`) — it multiplies launch speed directly, and
`response = 1/√10 ≈ 0.316` is exactly a 10x height loss, so dump its x/y and check x[0]≈0,
x[15]≈1; (b) `clamp_jump_velocity` at `ground_animation.rs:77-80` takes the *direction* of the
current (near-horizontal) velocity and only the *magnitude* of the jump velocity, which can
silently delete most of the Y.

**Cheap confirmation with no code change:** set the trainer `pop` multiplier. It scales `height`
before the subtraction, so if `pop ≈ 2` reproduces retail heights (rather than the ≈10 you would
need if the subtraction were innocent), the `current_height` diagnosis is confirmed.

**Current workaround.** `TEMPORARY_POP_HEIGHT_SCALE` = 10.0 in
`crates/skate-game/src/physics/ground_animation/board.rs`, passed as the non-retail
`GroundJumpInput::pop_height_scale` and applied to `remaining`. It is applied to the **real
launch**, so the trajectory, the selector packet, `max_y` and the audio field all agree — it
replaces an earlier audio-only scale that made landings sound right while the skater barely left
the ground. `skate-core`'s lifted routine stays retail-exact at `pop_height_scale = 1.0`.

**Fixed when.** An ollie measures 1.1–2.2 m in `physical.air.jump_height_200` with
`pop_height_scale` removed, and Treatment word 9 lands in 183–367 on an ordinary ollie.

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

1. **#1 (pop height)** — biggest gameplay-feel defect, most likely a single scale bug, and it
   removes a temporary hack.
2. **#2 (KnownAir)** — unblocks real air timing for everything, not just audio, and lets an
   audio-side workaround be deleted.
3. **#7 (broadphase)** — a reproducible correctness failure in contact generation, and it has a
   one-line repro.
4. **#3 (powerslide inputs)** — one trace comparison away from being isolated.
5. **#5 (surfaces)** — large, but the audio side is already ported and waiting.
6. **#4 (ragdoll/loose board)** and **#6 (NaN crash, once reproduced)**.
