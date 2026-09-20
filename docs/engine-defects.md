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

### Retail's actual ollie path, recovered 2026-09-20 — and why the premise now needs re-measuring

**Retail does not go 100 → 201.** An ordinary ollie runs **100 → 103 (GroundAnimation) → 201**, and
the trajectory is launched *and completed in the same frame* inside GroundAnimation's
`sub_82D33E30`, gated on `Toolkit_CalcGroundJump` (`sub_82D93618`) returning `JumpInfo+20 != 0`:

```
86.cpp:6996   bl 0x82d93618      Toolkit_CalcGroundJump
86.cpp:7022   beq cr6,...        not active -> ordinary ground forces
86.cpp:7080   bl 0x82d67848      LAUNCH
86.cpp:7085   bl 0x82d68800      COMMIT, same frame -> selector+9658 valid = 1
```

Then PostInput (`sub_82DB5588`) calls `sub_82D68800` again — nothing is pending, so it early-outs
and returns the **retained** `valid` — and stamps bit 0x400:
`91.cpp:62477 rlwimi r4,r5,10,21,21`. The 103 arm of the selector reads `+2572 == 1` and yields
`200 | bit` = 201 (`90.cpp:13258` → `90.cpp:14056`). `valid` is sticky: only `sub_82D67848`
(launch) and `sub_82D67228` (reset) clear it. PhysicsGround launches **only** on the animated-board
branch (`86.cpp:19001 lbz r8,2708(r31)`), which is a contact condition, not the ollie.

**This engine already matches all of that.** `ground_animation/board.rs:71-104` launches on
`jump.active` and calls `trajectory.update` on the very next lines, and `Trajectory::launch` fills
`pending_results` synchronously (`air_trajectory/mod.rs:50-54`), so the completion really does
happen in-frame. `complete_batch` sets `self.valid = true` at `selector.rs:278` **before** it
starts any second pass, and `launch_pass` does not clear it — so a pending second pass does not
take `valid` away. Both selector arms (`selector/ground.rs:33-35` for 100, `:107-109` for 103)
match retail's `field_2572 == 1 → air_variant_from_2468()`.

**So the "nothing publishes bit 10 except transiently" premise is not supported by the code as it
now stands, and it has not been re-measured since the workaround was written.** Note also that
rolling off an edge *correctly* yields PhysicsAir in both retail and here — `selector/ground.rs:42`
and `:110` hard-code it, exactly as retail does — so a roll-off test proves nothing about ollies,
and `tests/air_playback.rs` only ever asserts `category() == 200`, which 200/201/202 all satisfy.

**Do this before changing any physics.** Play, ollie, and read the new `st=` field:
`grep -o "AS st=[0-9]*" <trace> | sort | uniq -c`. If 201 appears on hops, this defect is stale
like #3 and #7 were and the only work left is deleting the `air_timing` workaround. If it never
does, the thing to instrument next is `jump.active` and the selector's `valid` at the moment
PostInput publishes, since every other link above is confirmed present.

## 3. Powerslide squeaks — **the "never fires" premise was wrong (corrected 2026-09-20)**

**Symptom as originally recorded.** No powerslide squeak sound, ever.

**That premise does not survive the traces.** Retail's gate is: both feet inside the deck box
(`+615`/`+616`), more than one wheel down, and `|deck tilt +264| × 114.5916 ≥ 15` (the threshold is
vault-loaded, `Hash_A129B33B4A2C7961`). Counting over the existing `logs/audio-trace-*.log`, the
gate passes on thousands of frames per session and `Class_Squeaks` **is posted**: 85 posts in
`audio-trace-20260920-122739.log`, 70 in `...-111039.log`, with thousands of `UP` redeliveries.
`|tilt264|` reaches 0.38–0.64 rad, comfortably past retail's 0.14–0.28 band, so the tilt input is
not short either. The gate and its four inputs are fine.

**What is actually unknown.** `tilt264` is the *steering* tilt (an exponential blend in
`TruckSteeringState::update`), not a slide-specific angle, so a passing gate does not prove the
squeak fired *during* PhysicsSlideGround. The `AS` trace line carried no state id, so the logs
could not answer it. That is now fixed: `AS` carries `st=` (State+16) and `cat=` (State+12) as of
this commit.

**How to finish it.** Play, powerslide, and filter the new trace for `st=101`:
`grep "AS st=101" <trace>` — then check whether those frames pass the gate and whether a
`Class_Squeaks` `PO` sits on the same frame number. If they do, the squeak is firing and the
remaining question is its rendered level (the posted speed word runs as low as 19/1000 at slide
onset while the turn word saturates at 1000); if they do not, compare the four fields on those
frames against the retail capture's `state.tsv`.

**Note for whoever picks this up:** the `DirectMixer` block in `player_audio.rs` also references
`Brd_Squeaks.abk` on a different gate, but it is dead code — the struct is never constructed. It
is not a confound.

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

## 7. Broadphase disagrees with a full scan — **resolved 2026-09-20: the test was wrong**

The engine is correct; the oracle was not. `tiled()` lays all 1024 fixture faces in the plane
`y=0` and the probe volumes sit at `y=0.25`. The recovered candidate producer `82ACEA30` offers
only the triangle's own face normal for point/segment/triangle volumes — no edge or vertex axes —
so a coplanar tile 10 km away in x still projects to a ~0.05 separation and survives `82ACE968`'s
`separation > fat + limit` gate. The linear reference therefore handed the narrow phase 1024 tiles
it would never see natively and collected 1025 contacts for a sphere that touches two triangles;
the hierarchy returned the correct 2. The mismatch only surfaced once `capacity = 100` truncated
the bogus set, which is why it looked like a retention bug.

That is native behaviour, not a defect: the native narrow phase is only ever reached through a
broadphase that has already rejected laterally, and **every in-game `BoardWorld` is built with
`with_query_metadata`**, so the per-triangle bounds cull is always active. `BoardWorld::new`
without metadata is test-only. Staggering the distant tiles in Y — separating them along the one
axis the SAT does test — makes the full scan a valid oracle again; linear and accelerated then
agree at 2/4/8/6 contacts for the sphere, capsule, rounded box and triangle. Fixed in the commit
that added this paragraph; 606 `skate-core` tests pass.

*Original entry, kept because the wrong diagnosis is instructive:*

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

## 8. Renderer crash on surface teardown — **seen once, did not reproduce**

`wgpu_hal::vulkan` panics with `Trying to destroy a SurfaceAcquireSemaphores that is still in use
by a SurfaceTexture` (`instance.rs:194`), taking the process down with exit code 0xC0000409.
Observed once at +24.8 s on 2026-09-20 (crash report
`report-1789928798791616000-15424.txt`); an immediate relaunch of the same binary ran fine, and the
four launches before it never hit it. Audio was up and healthy at the time (`player_sound ready`,
241 Treatment packets traced), so it is a renderer-side race on surface destruction, not audio.
Intermittent, so it needs repeated launches or a GPU-validation run to pin down.

## 9. Rail / grind landing sounds are not driven — **known gap, needs engine inputs**

Landing *onto* a rail makes no grind-onset sound. `ContactSound::GrindOnset` and
`ContactSound::PopRoll` are deliberately dropped in `player_audio/contact_voices.rs` rather than
played from an invented sample: retail routes them through a different subsystem — the
contact-sound manager at `[manager+668]`, fed by `sub_82496C58`'s per-material level interpolation
and `sub_82486EF0`'s 48-byte message. That path is also what carries retail's *continuous*
per-material impact level (see `docs/player-audio-retail-drivers.md` §9), so porting it fixes rail
landings and adds material-dependent landing weight at the same time. `sub_82486EF0`'s sink was
traced as far as `[g[0x830CFDC4]+668]->vfunc12`; the handler beyond that is not yet followed.

### Decoded 2026-09-20 — everything except the sink's own dispatch

**Why a rail landing is silent specifically.** `Contacts::step` suppresses the ordinary landing
voice during a grind (`contacts.rs:708`, `else if !airborne && !grinding`), so the grind onset is
the *only* sound retail plays for that event — and it is the one that is dropped. The two
suppressions compound into silence. Retail has no such gap because its onset always posts:
`sub_824BB0E0` has no zero-level guard, unlike the landing `sub_824BA630`, which skips its message
when either level is 0 (`11.cpp:26040-26050`).

**The game-side chain is already complete and correct**: contact geometry → `impact_speed_128` →
`AudioState.grind_impact_228` / `grind_material_692` / `grind_family_192` →
`ContactsOwner::grind_onset` → `VoiceRequest { material, family_base, tier }` →
`ContactVoices::play(GrindOnset, ..)`. Only the sink is missing.

**The message is fully decoded.** `sub_82486EF0` allocates 48 bytes and writes: `+0x00` material A
(the family base, 95 or 96), `+0x04` material B (the grind material, 143→10), `+0x08`/`+0x0C` the
impact tier, `+0x10` a `vec4` world position copied wholesale (from audio state `+48` for the grind
onset, `+144` for a landing), `+0x20`/`+0x24` the two 0…32767 levels, and three flag bytes at
`+0x28`/`+0x29`/`+0x2A`. Delivery is two-stage: `[manager+668]->vtable[+12](msg)` returns another
object and the message is handed to *its* `vtable[+12]` too.

**The levels are decoded too.** `sub_82496C58` returns
`low + (high - low) * (min(value, hi) - lo) / (hi - lo)`, truncated, with `(low, high)` chosen by a
(mode × material-category) table and defaulting to `(0, 32767)`; it returns 0 for a negative
material or mode 3. The tier selects which impact-speed window the interpolation runs over —
`[0.0, 0.25]` for tier 0 and `[0.25, 0.5]` for tier 1, both vault floats
(`086B66C3D4FFEE8F`, `B2ACAFDBCD963C93`) on the grind-material class. The Rust `VoiceRequest`
carries the tier but not those two bounds.

**What is still unknown is only the dispatch behind `vtable[+12]`** — i.e. how the material pair
picks the actual sound. Nothing in `sub_82486EF0` or `sub_824BB0E0` names a bank or a sample, which
is why `contact_voices.rs` is right to refuse to invent one. **Porting the onset means porting the
contact-sound manager, not adding an arm to `ContactVoicePlayer::selection`.**

### The sink is pinned to three words of `.data`/`.rdata` (2026-09-20)

The manager global `0x830CFDC4` is written in exactly one place, `sub_826D5500` (`28.cpp:52333`):
a 2192-byte object allocated and constructed by `sub_824845B8`. There is **no `stw rX,668`
anywhere**, because `+668` is not a named field: the constructor zero-fills a **14-entry pointer
array at `+656..+708`** (`9.cpp:44075-44088`), and `sub_82484FE8` (`9.cpp:45097`) fills it with
`modules[i] = sub_828DED90(i)`. So **the sink is `manager->modules[3]`** — array slot 3.

`sub_828DED90` (`46.cpp:57277`) is a type-id registry lookup over a vector at `0x830BBE00` of
12-byte descriptors (`+0` id, `+4` pool, `+8` create fn). The 14 descriptors are registered by
`sub_8248D2A0` (`9.cpp:64145`), whose addresses, create functions, object sizes and vtable
addresses are all recovered. **But the descriptors' id words live in `.data`, and the vtables'
contents live in `.rdata`, and the recompiler lifts only `.text`** — so neither "which class is
slot 3" nor "what function is at its `vtable[+12]`" is derivable from `generated/`.

Also corrected: **`sub_828AAF88` is not a fallback sink.** It returns the default
`EA::Allocator::ICoreAllocator` (`45.cpp:7080`), and the `loc_82486FBC` tail is
`GetDefaultAllocator()->Free(msg, 0)` — the null-sink path *destroys* the message. Model it as
"drop the event", not as a second delivery route. (Its twin `sub_828AAE90` is what allocated the
48 bytes, which is how the `vt[8]=Alloc` / `vt[12]=Free` slots were confirmed.)

**Three reads close this**, against a decrypted retail image at base `0x82000000`:
1. the id word at `0x8302CD1C` and at `0x8302CD80 + 12n` for n=0..12 — find the one equal to 3;
2. that descriptor's `+8` create fn, cross-checked against the recovered table, which names the
   class (each create fn passes a distinct allocation-tag string, so the class name is readable);
3. the word at `<that class's vtable> + 12` — the concrete `vtable[+12]`, which can then be pulled
   out of `generated/` with the usual awk and read directly.

**Getting a decrypted image is the blocker, and it is not hard in principle** — the retail
`default.xex` at `out/build/windows-release/game/` is `encryption=0001, compression=0002` (AES +
LZX), and the SDK ships `rex/system/xex_module.cpp` and `lzx.cpp` that do exactly this at runtime.
Note that `rexglue.exe dump-xex` **looks like a stub in the SDK build on this machine**: it exits 0
in 0.2 s and writes nothing, at any log level. Either fix that subcommand, add a tiny host that
calls the SDK's `XexModule` loader and writes the image out, or read the words from the running
recomp's memory.

### The sink is no longer blocked: the image dumps, and the chain resolves (2026-09-20, later)

`rexglue.exe dump-xex` is **not** a stub after all. Its own `_putenv_s` does not reach the loader,
but the loader reads `REXGLUE_DUMP_XEX_IMAGE_DIR` from the environment, so setting that variable
*externally* dumps the decrypted image:

```
cd C:\dev\skate3recomp\outuild\windows-release\game
REXGLUE_DUMP_XEX_IMAGE_DIR=<out> rexglue.exe dump-xex default.xex <out>
```

That writes `default_82000000_011B0000.bin` (18,546,688 bytes, base `0x82000000`), i.e. `.data`
and `.rdata` included. File offset = address − `0x82000000`.

With it, the three reads are done:

1. **Slot 3 is the FIRST descriptor**, `0x8302CD1C`: id 3, name pointer `0x82247544`, create fn
   `sub_824F16D0`. (An earlier inferred bijection guessed reg pos 4 — it was wrong. Read the ids,
   do not derive them.) Full table, descriptor → id: CD1C→3, CD80→10, CD8C→2, CD98→0, CDA4→1,
   CDB0→4, CDBC→5, CDC8→6, CDD4→7, CDE0→8, CDEC→9, CDF8→11, CE04→12, CE10→13.
2. **The class is `CSTATEMGR_Collision`** (the name string at `0x82247544`).
3. **Its vtable is `0x822FD4FC`**, read from the create function itself
   (`lis r9,-32208; addi r8,r9,-11012; stw r8,0(r3)` = `0x82300000 − 0x2B04`) rather than from an
   inherited table, which had it 0x10000 too high. Slots: +0 `824F1B70`, +4 `828DE848`,
   +8 `824F17B8`, **+12 `824F1818`**, +16 `828DEE58`, +20 `828DEED8`, +24 `828DEF38`,
   +28 `824F16B0`, +32 `824F16C0`, +36 `824F88A8`.

**`sub_824F1818` (hop 1) is a voice-slot router, not the voice starter.** It walks the linked list
at `[this+16]` through `[node+4]`, keeps the node with the lowest signed `[node+64]` (a priority or
age), calls `[node->vtable+28](node)` on the winner and returns it. `sub_82486EF0` then delivers
the 48-byte message to *that node's* `vtable[+12]` — hop 2, which is where the material pair and
the two levels finally choose a sound.

**Next step, and it is now ordinary work rather than a dead end:** find what populates
`[manager+16]`, take a node's vtable out of the dumped image the same way, and read its `+12`.
Everything upstream is pinned, and `sub_824F16D0` shows the object is 28 bytes with `+12`, `+16`,
`+20` zeroed, `+4`/`+8` set to the `0x82165A10` float (0.0) and `+24` a zeroed byte.

**One confirmed, independent port gap while you are in there.** `ContactLatches::grind_gate_124` is
read at `contacts.rs:667` but **never written anywhere in the tree**. Retail writes it at
`sub_824BB0E0`'s tail from the constant at `0x8209975C` — verified directly in the lifted asm
(`lfs f0,124(r3)` at the head, `stfs f0,124(r25)` at the tail). That is the onset's cooldown /
re-arm; without it, once the onset *is* driven it will retrigger on every grind edge with no
spacing. Fixing this now is pointless while the sound is silent, but it must land with the sink.

As of this commit the two dropped sounds also **report themselves once per run** through the
existing `SKATE_PLAYER_AUDIO contact_voice_unavailable` channel instead of disappearing silently.

---

## Suggested order (updated 2026-09-20)

**Three of the nine entries turned out to rest on premises the evidence does not support** — #1
(2026-09-20, earlier), then #7 and #3 here. A fourth, #2, is now in the same position. The pattern
is consistent enough to be a rule: **re-measure the premise before writing code against it.**

**One playtest answers two of these at once.** Play, ollie a few times, powerslide a few times,
with `SKATE_AUDIO_TRACE=logs\...`, then:

- `grep -o "AS st=[0-9]*" <trace> | sort | uniq -c` → does an ollie reach **201**? (#2)
- `grep "AS st=101" <trace> | head` → does the squeak gate pass during a real powerslide, and is
  there a `Class_Squeaks` `PO` on the same frame? (#3)

Both questions were unanswerable before this session because `AS` carried no state id; it does now.

1. **The playtest above** — it is the gate on both #2 and #3, and costs one run.
2. **#5 (surfaces)** — large, but the audio side is already ported and waiting, and it is the
   single biggest audible gap now that the impacts are measured-correct: every surface currently
   sounds like concrete.
3. **#9 (rail/grind landings)** — everything is decoded except the contact-sound manager's
   `vtable[+12]` dispatch (see above). That one subsystem unlocks rail landings *and* the
   continuous material-dependent landing weight in `player-audio-retail-drivers.md` §9. Land the
   `grind_gate_124` re-arm with it.
4. **#4 (ragdoll/loose board)**, **#6 (NaN crash)** and **#8 (renderer race)** — all need a repro
   before they can be chased.

Resolved and needing nothing: **#1**, **#7**, and the offboard static matching-group bug below.

## 10. Static geometry was hidden from offboard queries — **fixed 2026-09-20**

Not previously listed; found because two tests contradicted each other. `skate_world.rs` filed the
RWCM cluster's packed unit group on `QueryMesh::matching_group`. Since the filter is
`a == -1 || b == -1 || a == b`, that made every offboard ground and line query **reject static
geometry unless the querying actor's matching id happened to equal the cluster's group**.

Retail writes **-1** there: the mesh-record constructor `sub_8276CB18` takes matchingID in `r8`
(`stw r8,180`), and every call site passes a constant `li r8,-1` except one pass-through that
reads an actor/unit object's `+40` — never cluster data. The static path pairs that -1 with the
`li r7,0` / `li r6,0` this code already reproduced as `geometry: 0` / `rejection_flags: 0`. The
cluster group is per-*triangle* in retail: `sub_82AC8A68` stores it at triangle+84, beside the
surface code at +88 that `packed_surfaces` carries. The group is still used to partition clusters
into meshes; only the misfiling is gone.

Worth a playtest look: anything offboard (walking, bailing, ragdoll) that seemed to pass through
static world geometry may simply have been unable to see it.
