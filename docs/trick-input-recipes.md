# Trick input recipes

`docs/trick-reachability.md` says *whether* a scorable has a crediting path. It does not say how
to perform it. This does, for the families where the answer is not obvious, decoded from the
authored `ActionGraphIncludes/` and confirmed by the playback tests named against each one.

## The rule that governs every board-adjust grab: stick before trigger

This one cost a full debugging session, so it is first.

The air ActionGraph selects **one** active leaf. `GetNextState` (82C140D8, ported in
`skate-core/src/graph/selection.rs`) descends from the root taking the *first* child whose
expression activates, and `ActionGraphIncludes/air.xml` orders the air state's children
`HandPlant`, `Grabbing`, `BoardAdjusting`, ... `Grabbing`'s `T_Grab.xml` children are gated on
nothing but `LeftAirGrab` / `RightAirGrab` -- there is no angle constraint on them at all.

So a trigger held from the first airborne frame always lands in `Grabbing.LeftGrab`, a plain
`fsgrab`, and `BoardAdjusting` is never entered. The intents are all perfectly correct while this
happens -- `BoardAdjustAngle` sits at exactly pi and `BoardAdjustMag` at 1.0 -- which makes it
look like a defect in the board-adjust branch. It is not.

`BoardAdjustUp.xml`'s `Idle` requires only `BoardAdjustMag`, and carries
`<transition target="TailGrab"/>`. So the authored order is:

1. **Stick first.** With no trigger held, `Grabbing` has no activatable child, so the selected
   leaf becomes `BoardAdjusting.Up.Idle` (or `.Down.Idle`).
2. **Then the trigger.** `search_transitions` walks up from that leaf, finds `Idle`'s transition,
   and enters `TailGrab` from *inside* the board-adjust branch.

Six ticks of stick lead is enough (`STICK_LEAD` in `tests/fingerflip_playback.rs`).

## Grabs

`BoardAdjustAngle` is `atan2(x, -y)` over the raw right stick (`trick_intentions::produce`), so
raw `+y` is the up branch and `-y` the down branch. The up branch claims
`abs(angle) >= 2.355`, the down branch `abs(angle) <= 0.785`.

| Trick | id | Right stick | Trigger | Authored branch |
|---|---:|---|---|---|
| `tailgrab` | 82 | up (`[0, +32767]`) | left | `BoardAdjusting.Up.TailGrab` |
| `seatbeltgrab` | 190 | up | right | `BoardAdjusting.Up.SeatbeltGrab` |
| `nosegrab` | 74 | down (`[0, -32767]`) | right | `BoardAdjusting.Down.NoseGrab` |
| `crailgrab` | 187 | down | left | `BoardAdjusting.Down.CrailGrab` |
| `rocketair` | 77 | down | **both** | `BoardAdjusting.Down.RocketAir` |
| `fsgrab` | 62 | centred | left | `Grabbing.LeftGrab` |
| `bsgrab` | 63 | centred | right | `Grabbing.RightGrab` |
| `dblgrab` | 61 | centred | both | `Grabbing.DBLGrab` |

Mirrored (goofy) stance swaps every left/right trigger above; each authored expression pairs
`HasAGIntent <hand>AirGrab` with `IsMirrored`.

The `_left` / `_right` / `_up` / `_down` directional variants are not separate inputs. Holding
the grab and then tweaking the stick moves `BoardAdjustAngle`, and `ScoringGrabs`'
`select_grab_score` picks the variant whose authored angular window contains it, falling back to
the base name inside a 0.5 dead zone.

Proven by `tests/fingerflip_playback.rs::held_grabs_reach_their_authored_names`.

## Grab shove-its (fingerflips)

Hold the grab as above, then flick an authored scoop **without releasing the trigger**. The
scoops live in their own recognizer, `skater_fingerflip.pat`, which authors exactly three
patterns: `Fingerflip`, `FS_Varial` and `BS_Varial`.

| Trick | id | Grab | Scoop | Authored branch |
|---|---:|---|---|---|
| `tailgrab_fingerflip` | 176 | tailgrab | `Fingerflip` | `Up.Grab.Grabbing.FingerFlip` |
| `nosegrab_fingerflip` | 175 | nosegrab | `Fingerflip` | `Down.Grab.Grabbing.FingerFlip` |
| `seatbelttonose_fingerflip` | 255 | seatbeltgrab | `BS_Varial` | `Up...SeatbeltGrab.SeatbeltFingerFlipShuv` |
| `crailtotail_fingerflip` | 256 | crailgrab | `BS_Varial` | `Down...CrailGrab.CrailFingerFlipShuv` |
| `fsgrab_fingerflip` | 173 | fsgrab | `Fingerflip` | `GrabsTweaks.FSGrab.FingerFlip` |
| `bsgrab_fingerflip` | 174 | bsgrab | `Fingerflip` | `GrabsTweaks.BSGrab.FingerFlip` |

The last two hang off `Grabbing`, not `BoardAdjusting`, so they do not need the stick lead.

The two `*to*` entries genuinely cross branches: the seatbelt shuv takes
`<transition target="Down.NoseGrab"/>` and the crail shuv takes `<transition target="Up.TailGrab"/>`,
so the grab the skater is holding changes mid-air. The animations confirm it --
`F_FLIP_SEATBELT_TO_NOSE_GRAB` then `B_NOSEGRAB_CYC`.

Both `FingerFlipShuv` states also accept the late-shuvit gestures `L_FS_Shuvit` / `L_BS_Shuvit`
from `skater_air.pat`, so a late shove-it out of a grab reaches the same trick.

Because the recognizer is fed the held grab direction every tick, the hold has already walked the
scoop's first coordinate; the flick only has to complete it. The test picks whichever authored
variant starts nearest the held point, after the input manager's Y negation.

Proven by `grab_fingerflips_reach_their_authored_tricks` and
`grab_to_grab_fingerflip_shuvs_cross_the_board_adjust_branches`.

## One-foot and no-foot, out of a held grab

Every board-adjust grab carries the same three children, gated on the push buttons rather than on
the stick:

| Addition | Intent | Button |
|---|---|---|
| `OneFootRight` | `RightPush` | A |
| `OneFootLeft` | `LeftPush` | X |
| `NoFootAirWalk` | `Dismount`, or both pushes at once | B |

Out of the tailgrab that reaches `tailgrab_airwalk` (172); out of the nosegrab,
`nosegrab_airwalk` (254). `air_playback.rs::airborne_tailwalk_from_raw_controller_reaches_stock_cycle`
targets the first of these, but it rebuilds `BoardWorld` for a deeper landing and dies on
`Canonical world has no authored query metadata` before it gets there -- the same hazard
`docs/flip-ladder.md` records. Prefer the velocity-boost approach in `fingerflip_playback.rs`,
which leaves the canonical course's authored query metadata intact.

## Late flips

Ollie, then run a `skater_air.pat` scoop in the air: `L_F_Kickflip`, `L_B_Kickflip`,
`L_F_Heelflip`, `L_B_Heelflip`, `L_FS_Shuvit`, `L_BS_Shuvit`, and no others. These never enter
the 270-row trick mapping; they reach the MotionGraph as bare intents matched by `HasIntent`.
Covered by `flip_playback.rs::air_scoops_reach_the_authored_late_flips`, and see that file's doc
for why there is no late double, triple or quad.
