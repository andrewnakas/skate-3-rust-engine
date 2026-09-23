//! The grab fingerflips -- "grab shove-its" -- and the grabs they are launched from.
//!
//! A grab is a held trigger plus a right-stick direction, and the authored `BoardAdjust`
//! branches turn the two into a named grab: `ActionGraphIncludes/BoardAdjustUp.xml` claims
//! `abs(BoardAdjustAngle) >= 2.355` and gives `LeftAirGrab` the tail and `RightAirGrab` the
//! seatbelt; `BoardAdjustDown.xml` claims `<= 0.785` and gives `RightAirGrab` the nose,
//! `LeftAirGrab` the crail and both triggers the rocket air. `BoardAdjustAngle` is
//! `atan2(x, -y)` (`trick_intentions::produce`), so raw right-stick `+y` is the up branch and
//! `-y` the down branch.
//!
//! From inside a grab, an authored `Fingerflip` scoop on the same stick reaches the
//! `.Grab.Grabbing.FingerFlip` child, whose `ScoringTrick` leaf names `tailgrab_fingerflip` /
//! `nosegrab_fingerflip`. The flick therefore has to be played *while the trigger is still
//! held*, which is what separates these from the ordinary air scoops in `flip_playback.rs`.
use super::*;
use skate_core::animation::skeleton_input::name::encode;

/// The grabs reachable from a single held trigger and a cardinal stick hold, with the trick the
/// authored graph names for each. Sites are from `docs/trick-reachability.md`.
#[test]
#[ignore = "requires private stock graphs and animation assets"]
fn held_grabs_reach_their_authored_names() {
    for (hands, stick, grab) in [
        (&[0][..], UP, "tailgrab"),
        (&[1][..], UP, "seatbeltgrab"),
        (&[1][..], DOWN, "nosegrab"),
        (&[0][..], DOWN, "crailgrab"),
        // Both triggers: the down branch's own child, not the stance-free double grab.
        (&[0, 1][..], DOWN, "rocketair"),
        // Centred stick keeps `BoardAdjustMag` at zero, so `Grabbing` wins the descent and the
        // stance-free double grab is what a two-trigger hold reaches.
        (&[0, 1][..], CENTRE, "dblgrab"),
    ] {
        let run = replay(hands, stick, None, 300);
        assert!(
            run.grabs.iter().any(|g| g.starts_with(grab)),
            "{grab}: hands={hands:?} stick={stick:?} never published it; saw grabs={:?} tricks={:?} over {:?}",
            run.grabs,
            run.tricks,
            run.animations
        );
    }
}

/// The tail and nose grab shove-its: hold the grab, then flick the authored `Fingerflip` scoop
/// without letting the trigger go.
#[test]
#[ignore = "requires private stock graphs and animation assets"]
fn grab_fingerflips_reach_their_authored_tricks() {
    for (hands, stick, trick) in [
        (&[0][..], UP, "tailgrab_fingerflip"),
        (&[1][..], DOWN, "nosegrab_fingerflip"),
    ] {
        let run = replay(hands, stick, Some("Fingerflip"), 300);
        assert!(
            run.tricks.iter().any(|t| t == trick),
            "{trick}: the held grab plus scoop never published it; saw tricks={:?} grabs={:?} over {:?}",
            run.tricks,
            run.grabs,
            run.animations
        );
    }
}

/// The grab-to-grab shove-its. A varial scoop out of the seatbelt takes the authored
/// `<transition target="Down.NoseGrab"/>` and scores `seatbelttonose_fingerflip`; out of the
/// crail it takes `<transition target="Up.TailGrab"/>` and scores `crailtotail_fingerflip`.
/// These are the two that cross from one board-adjust branch into the other.
#[test]
#[ignore = "requires private stock graphs and animation assets"]
fn grab_to_grab_fingerflip_shuvs_cross_the_board_adjust_branches() {
    for (hands, stick, scoop, trick) in [
        (&[1][..], UP, "BS_Varial", "seatbelttonose_fingerflip"),
        (&[0][..], DOWN, "BS_Varial", "crailtotail_fingerflip"),
    ] {
        let run = replay(hands, stick, Some(scoop), 300);
        assert!(
            run.tricks.iter().any(|t| t == trick),
            "{trick}: the {scoop} scoop never published it; saw tricks={:?} grabs={:?} over {:?}",
            run.tricks,
            run.grabs,
            run.animations
        );
    }
}

const UP: [i16; 2] = [0, 32767];
const DOWN: [i16; 2] = [0, -32767];
const CENTRE: [i16; 2] = [0, 0];

/// Air time for the pop, the grab settle and the scoop. The flat course leaves about 0.77 s on
/// its own (see `docs/flip-ladder.md`), which the grab alone consumes.
const BOOST: f32 = 13.0;
/// Ticks of stick-only hold before the trigger, so the board-adjust branch is the selected leaf
/// when the grab arrives.
const STICK_LEAD: u32 = 6;
/// Ticks of held grab before the scoop, so `.Grab.Grabbing` is entered and the cycle is running.
const SETTLE: u32 = 22;
/// Ticks per authored coordinate, comfortably inside the recognizer's culling window.
const SCOOP_STEP: u32 = 3;

#[derive(Default)]
struct Outcome {
    animations: Vec<String>,
    tricks: Vec<String>,
    grabs: Vec<String>,
}

/// Every scorable the graph can publish from a grab, so an unexpected one is reported by name
/// rather than as a silent miss. Sourced from `scoring/catalog.rs`.
const WATCHED: [&str; 24] = [
    "tailgrab",
    "tailgrab_left",
    "tailgrab_right",
    "tailgrab_fingerflip",
    "tailgrab_airwalk",
    "nosegrab",
    "nosegrab_left",
    "nosegrab_right",
    "nosegrab_fingerflip",
    "nosegrab_airwalk",
    "crailgrab",
    "crailgrab_left",
    "crailgrab_right",
    "crailtotail_fingerflip",
    "seatbeltgrab",
    "seatbeltgrab_left",
    "seatbeltgrab_right",
    "seatbelttonose_fingerflip",
    "rocketair",
    "fsgrab",
    "bsgrab",
    "fsgrab_fingerflip",
    "bsgrab_fingerflip",
    "dblgrab",
];

fn replay(hands: &[usize], stick: [i16; 2], scoop: Option<&str>, ticks: u32) -> Outcome {
    let root =
        std::path::PathBuf::from(std::env::var_os("SKATE3_ASSET_ROOT").expect("SKATE3_ASSET_ROOT"));
    let assets = skate_data::GameAssets::load(&root).unwrap();
    let graphs = crate::graph_runtime::StockGraphs::load(&root, &assets).unwrap();
    let mut physics = GamePhysics::load_with_terrain(&root, ground::Terrain::Course).unwrap();
    let mut skater = SkaterRuntime::load(&root, &graphs, &physics, "normal").unwrap();
    let mut camera = crate::camera::CameraRuntime::load(&root).unwrap();
    let mut controller = crate::input::ControllerInput::default();
    let mut controls = PlayerControls::load(&root).unwrap();

    let ollie = authored("skater.pat", &root, "Ollie");
    // GameInputManager 82696030 negates the mapped Y before matching, so the recognizer sees the
    // held grab direction with its Y flipped. Pick the authored variant that starts nearest that
    // point: the hold has already walked the pattern's first coordinate, so the flick only has to
    // complete it.
    let held = [stick[0] as f32 / 32767., -(stick[1] as f32) / 32767.];
    let scoop: Vec<[f32; 2]> = scoop
        .map(|name| {
            let mut variants = authored_all("skater_fingerflip.pat", &root, name);
            variants.sort_by(|a, b| {
                distance(a[0], held)
                    .partial_cmp(&distance(b[0], held))
                    .unwrap()
            });
            variants.swap_remove(0)
        })
        .unwrap_or_default();
    let watched: Vec<(&str, _)> = WATCHED.iter().map(|n| (*n, encode(n.as_bytes()))).collect();
    let sample = |p: [f32; 2]| [(p[0] * 32767.) as i16, (-p[1] * 32767.) as i16];

    let mut outcome = Outcome::default();
    let mut air_ticks = 0u32;
    for tick in 0..ticks {
        if tick == 60 {
            // A rolling approach, without a steering input that would also spin.
            for body in physics.board.bodies_mut() {
                body.rates.linear_velocity = Vector3::new(0., 0., 8.);
            }
            for body in skater.skeleton.bodies_mut() {
                body.rates.linear_velocity = Vector3::new(0., 0., 8.);
            }
        }
        if tick == 123 {
            for body in physics.board.bodies_mut() {
                body.rates.linear_velocity.y += BOOST;
            }
            for body in skater.skeleton.bodies_mut() {
                body.rates.linear_velocity.y += BOOST;
            }
        }
        let airborne = skater.player_state.current().category() == 200;
        let (right, triggers) = if !airborne {
            // Crouch on the scoop's first coordinate, then flick it: the ordinary ground pop.
            let right = if (100..112).contains(&tick) {
                sample(ollie[0])
            } else if (112..118).contains(&tick) {
                sample(ollie[ollie.len() - 1])
            } else {
                [0; 2]
            };
            (right, [0u8; 2])
        } else {
            // Stick before trigger. The air state selects a single leaf and `Grabbing` precedes
            // `BoardAdjusting` in document order, so a trigger held from the first airborne
            // frame lands in `Grabbing.LeftGrab` (a plain fsgrab) and the board-adjust branch is
            // never reached. Leading with the stick selects `BoardAdjusting.Up.Idle`, whose only
            // precondition is `BoardAdjustMag`, and the trigger then takes its authored
            // `<transition target="TailGrab"/>` from inside the branch.
            let mut triggers = [0u8; 2];
            if air_ticks >= STICK_LEAD {
                for &hand in hands {
                    triggers[hand] = 255;
                }
            }
            let right = if air_ticks < SETTLE || scoop.is_empty() {
                stick
            } else {
                let step = ((air_ticks - SETTLE) / SCOOP_STEP) as usize;
                // Hold the scoop's last coordinate rather than centring: releasing to centre
                // drops the grab's own BoardAdjustMag and exits the branch mid-trick.
                scoop
                    .get(step)
                    .or_else(|| scoop.last())
                    .copied()
                    .map(sample)
                    .unwrap_or(stick)
            };
            (right, triggers)
        };
        controller.sample_raw_for_test(skate_core::input::xbox::XboxState {
            buttons: 0,
            triggers,
            left: [0; 2],
            right,
        });
        let mut actions = controller.player_actions();
        controls.update(
            &mut actions,
            physics.settings.step.simulation.time_step,
            physics.settings.input_magnitude_threshold,
            skater.player_input.physical.scoring.capabilities_204,
        );
        controls.publish_gestures(
            physics.animation_profile.physics_mode,
            skater.player_input.physical.state.state_16,
        );
        frame::advance(
            &mut physics,
            &mut skater,
            &mut controls,
            &graphs,
            &mut actions,
            true,
            &mut camera,
        )
        .unwrap_or_else(|e| panic!("tick{tick}: {e}"));
        if skater.player_state.current().category() == 200 {
            air_ticks += 1;
        }

        let motion = &skater.animation.motion;
        if std::env::var_os("TRACE_GRAB").is_some() && airborne && air_ticks % 4 == 0 {
            let ag = |n: &str| controls.action_intents.get(n).copied();
            let mg = |n: &str| motion.animation.motion_intents.get(n).copied();
            eprintln!(
                "air{air_ticks:>3} AG angle={:?} mag={:?} L={:?} R={:?} | MG up={:?} down={:?} tail={:?} nose={:?} fs={:?} bs={:?} | anim={:?}",
                ag("BoardAdjustAngle"),
                ag("BoardAdjustMag"),
                ag("LeftAirGrab"),
                ag("RightAirGrab"),
                mg("BoardAdjustUp"),
                mg("BoardAdjustDown"),
                mg("TailGrab"),
                mg("NoseGrab"),
                mg("FSGrab"),
                mg("BSGrab"),
                motion.animation.current_name
            );
        }
        if let Some(name) = motion.animation.current_name.as_deref()
            && outcome.animations.last().map(String::as_str) != Some(name)
        {
            outcome.animations.push(name.to_owned());
        }
        if let Some(published) = motion.score_packet.trick_names.first
            && let Some((name, _)) = watched.iter().find(|(_, e)| *e == published)
            && outcome.tricks.last().map(String::as_str) != Some(*name)
        {
            outcome.tricks.push((*name).to_owned());
        }
        if let Some((published, _)) = motion.score_packet.grab
            && let Some((name, _)) = watched.iter().find(|(_, e)| *e == published)
            && outcome.grabs.last().map(String::as_str) != Some(*name)
        {
            outcome.grabs.push((*name).to_owned());
        }
    }
    eprintln!(
        "hands={hands:?} stick={stick:?}: grabs={:?} tricks={:?}\n  animations={:?}",
        outcome.grabs, outcome.tricks, outcome.animations
    );
    outcome
}

fn distance(point: [f32; 2], to: [f32; 2]) -> f32 {
    (point[0] - to[0]).hypot(point[1] - to[1])
}

fn authored_all(file: &str, root: &std::path::Path, name: &str) -> Vec<Vec<[f32; 2]>> {
    let patterns =
        skate_data::gesture_patterns::load(&root.join("private/stock/data/joystick").join(file))
            .unwrap();
    let found: Vec<Vec<[f32; 2]>> = patterns
        .into_iter()
        .filter(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.points)
        .collect();
    assert!(!found.is_empty(), "authored {name} pattern in {file}");
    found
}

fn authored(file: &str, root: &std::path::Path, name: &str) -> Vec<[f32; 2]> {
    authored_all(file, root, name).swap_remove(0)
}
