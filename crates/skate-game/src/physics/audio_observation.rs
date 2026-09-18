//! Publish one immutable player/board audio observation for every completed physics tick.
//!
//! The observation has no sound-selection policy. In particular, the physics wheel-surface vote
//! is carried as raw evidence until it is matched to the retail audio-material path.

use bevy::prelude::*;
use skate_core::math::Vector3;

use super::{GamePhysics, SkaterRuntime};

#[derive(Resource)]
pub(crate) struct AudioObservationCursor {
    published_tick: Option<u64>,
    last_surface: Option<u32>,
    last_powersliding: bool,
    last_trick_id: Option<usize>,
    last_state: Option<u32>,
    last_board_vertical_speed: f32,
    last_rider_vertical_speed: f32,
    trace: bool,
}

impl Default for AudioObservationCursor {
    fn default() -> Self {
        Self {
            published_tick: None,
            last_surface: None,
            last_powersliding: false,
            last_trick_id: None,
            last_state: None,
            last_board_vertical_speed: 0.0,
            last_rider_vertical_speed: 0.0,
            trace: std::env::var_os("SKATE_AUDIO_OBSERVE").is_some(),
        }
    }
}

fn speed(value: Vector3) -> f32 {
    value
        .x
        .mul_add(value.x, value.y.mul_add(value.y, value.z * value.z))
        .sqrt()
}

fn raw_vector(raw: [u32; 4]) -> [f32; 3] {
    [f32::from_bits(raw[0]), f32::from_bits(raw[1]), f32::from_bits(raw[2])]
}

/// The native PhysOut fields `sub_827A1B78` packs for the audio-state bridge `sub_824B0DA8`,
/// read from the engine's published native records.
fn retail_inputs(skater: &SkaterRuntime) -> crate::skate_audio::RetailAudioInputs {
    let physical = &skater.player_input.physical;
    crate::skate_audio::RetailAudioInputs {
        ground_speed: physical.skateboard.scalar_164,
        com_velocity: raw_vector(physical.reckoning.vector_16),
        position: raw_vector(physical.reckoning.vector_64),
        wheel_count: physical.collision.wheel_count_0 & 7,
        air_time_in_state: physical.air.time_in_state_176,
        air_time_until_landing: physical.air.scalar_184,
        air_jump_height: physical.air.jump_height_200,
        state_flags: skater.player_state.state_flags,
        offboard_feet: physical.off_board.flags_306_307.map(|flag| flag != 0),
        footplant: [
            physical.air.footplant_left_449 != 0,
            physical.air.footplant_right_450 != 0,
        ],
        footstep_strength: skater.animation_input.extra.footstep_strength,
    }
}

fn landing_state_edge(previous: Option<u32>, current: u32) -> bool {
    matches!(previous, Some(103 | 200 | 201 | 202)) && current == 100
}

/// Copy the completed snapshot only once, even if presentation runs more than once between fixed
/// simulation ticks.
pub(crate) fn publish(
    physics: Res<GamePhysics>,
    skater: Res<SkaterRuntime>,
    mut cursor: ResMut<AudioObservationCursor>,
    mut observations: MessageWriter<crate::skate_audio::PlayerAudioObservation>,
) {
    let Some(output) = physics.exchange.output() else {
        return;
    };
    if cursor.published_tick == Some(output.tick) {
        return;
    }
    cursor.published_tick = Some(output.tick);
    let wheel_surface = super::ground_runtime::active_audio_surface(&physics.riding);
    let surface_changed = cursor.last_surface != Some(wheel_surface);
    cursor.last_surface = Some(wheel_surface);
    let trick = skater
        .animation
        .motion
        .score_packet
        .trick_names
        .first
        .and_then(|name| skater.scoring.data.by_name(name));
    let grind = &skater.player_input.physical.grinds;
    let state = output.state as u32;
    let landed = output.landed || landing_state_edge(cursor.last_state, state);
    cursor.last_state = Some(state);
    let landing_impact_speed = if landed {
        (-cursor.last_board_vertical_speed)
            .max(-cursor.last_rider_vertical_speed)
            .max(-output.board_linear_velocity.y)
            .max(-output.rider_linear_velocity.y)
            .max(0.0)
    } else {
        0.0
    };
    cursor.last_board_vertical_speed = output.board_linear_velocity.y;
    cursor.last_rider_vertical_speed = output.rider_linear_velocity.y;
    let foot_surface = if output.state as u32 / 100 == 5 {
        let left = skater.player_input.processed.left_surface_2596 & 0x7f;
        let right = skater.player_input.processed.right_surface_2600 & 0x7f;
        if left != 0 { left } else { right }
    } else {
        wheel_surface
    };
    let landing_quality = skater.landing_quality;
    let (landing_clean, landing_sketchy) =
        skater.scoring.classify_landing_audio(landing_quality);
    let observation = crate::skate_audio::PlayerAudioObservation {
        tick: output.tick,
        state,
        board_speed: speed(output.board_linear_velocity),
        rider_speed: speed(output.rider_linear_velocity),
        grounded: output.grounded,
        grinding: output.state.is_grind(),
        grind_family: grind.words_136_140[0],
        grind_substate: grind.words_136_140[1],
        grind_audio_surface: grind.audio_surface_216,
        grind_impact_speed: grind.impact_speed_128,
        wiping_out: output.wiping_out,
        landed,
        landing_impact_speed,
        landing_clean,
        landing_sketchy,
        landing_type: landing_quality.landing_type_96,
        landing_spin: landing_quality.spin_92,
        landing_sideways_speed: landing_quality.sideways_speed_84,
        footstep_strength: output.footstep_strength,
        footstep_bone: output.footstep_bone,
        foot_push_speed: output.foot_push_speed,
        feet_supported: output.feet_supported,
        foot_surface,
        contact_count: output.contact_count,
        powersliding: skater.animation.motion.is_power_sliding,
        trick_id: trick.map(|definition| definition.metadata.id),
        trick_identifier: trick.map(|definition| definition.identifier.to_owned()),
        animation_name: skater.animation.motion.animation.current_name.clone(),
        riding_switch: skater.animation.packet.riding_switch,
        riding_fakie: skater.animation.packet.riding_fakie,
        nollie: skater.animation.packet.weight_forwards,
        wheel_surface,
        events: output.events.clone(),
        retail: retail_inputs(&skater),
    };
    let powerslide_changed = cursor.last_powersliding != observation.powersliding;
    let trick_changed = cursor.last_trick_id != observation.trick_id;
    cursor.last_powersliding = observation.powersliding;
    cursor.last_trick_id = observation.trick_id;
    if cursor.trace
        && (surface_changed
            || powerslide_changed
            || trick_changed
            || observation.footstep_strength != 0.0
            || observation.foot_push_speed != 0.0
            || observation.landed
            || !observation.events.is_empty())
    {
        eprintln!(
            "skate-audio-observe tick={} state={} board_speed={:.3} rider_speed={:.3} grounded={} wiping_out={} powersliding={} contacts={} wheel_surface={} foot_surface={} grind={}/{}/{} grind_impact={:.3} landing_impact={:.3} landing_quality={}/{}/{} spin={:.3} side={:.3} trick={:?}/{:?} animation={:?} switch={} fakie={} nollie={} footstep={} foot_bone={} push={} events={:?}",
            observation.tick,
            observation.state,
            observation.board_speed,
            observation.rider_speed,
            observation.grounded,
            observation.wiping_out,
            observation.powersliding,
            observation.contact_count,
            observation.wheel_surface,
            observation.foot_surface,
            observation.grind_family,
            observation.grind_substate,
            observation.grind_audio_surface,
            observation.grind_impact_speed,
            observation.landing_impact_speed,
            observation.landing_type,
            observation.landing_clean,
            observation.landing_sketchy,
            observation.landing_spin,
            observation.landing_sideways_speed,
            observation.trick_id,
            observation.trick_identifier,
            observation.animation_name,
            observation.riding_switch,
            observation.riding_fakie,
            observation.nollie,
            observation.footstep_strength,
            observation.footstep_bone,
            observation.foot_push_speed,
            observation.events,
        );
    }
    observations.write(observation);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_uses_all_three_physics_axes() {
        assert_eq!(speed(Vector3::new(2.0, 3.0, 6.0)), 7.0);
    }

    #[test]
    fn air_to_ground_edge_is_an_audio_landing_even_without_a_physics_event() {
        assert!(landing_state_edge(Some(201), 100));
        assert!(landing_state_edge(Some(200), 100));
        assert!(!landing_state_edge(Some(101), 100));
        assert!(!landing_state_edge(Some(201), 403));
    }
}
