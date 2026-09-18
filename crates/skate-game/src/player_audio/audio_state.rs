//! The retail per-frame audio state: a port of the bridge `sub_824B0DA8`, which copies the
//! per-skater PhysOut record `sub_827A1B78` builds into the audio state every player-sound
//! component reads (the component's `+32`/`+36` pointer). Field names carry the audio-state
//! offset; `docs/player-audio-retail-drivers.md` §1 lists each one's native source.
//!
//! Only fields whose native source the engine publishes are here. A family that needs a field
//! the engine does not compute yet stays off rather than reading an invented value.

use crate::skate_audio::RetailAudioInputs;

/// Player state byte `offset` (52..=87) from the published state flags.
fn state_flag(inputs: &RetailAudioInputs, offset: usize) -> bool {
    inputs.state_flags[offset - 52]
}

fn length(v: [f32; 3]) -> f32 {
    v[0].mul_add(v[0], v[1].mul_add(v[1], v[2] * v[2])).sqrt()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AudioState {
    /// +96: COM velocity (SystemReckoning+16).
    pub com_velocity_96: [f32; 3],
    /// +112: the previous frame's +96.
    pub com_velocity_prev_112: [f32; 3],
    /// +128: +96 minus the previous frame's +96.
    pub com_velocity_delta_128: [f32; 3],
    /// +200: wheel count, `(R152 >> 20) & 7`.
    pub wheel_count_200: u32,
    /// +208: board ground speed (SkateboardMotion+164), m/s. Rolling, rattle, grind, seams, skid,
    /// squeaks and the MixMap speed inputs all read this, not the 3-D deck speed.
    pub ground_speed_208: f32,
    /// +212: |COM velocity|; +216 the previous frame's +212.
    pub com_speed_212: f32,
    pub com_speed_prev_216: f32,
    /// +220: time scale. The engine has no slow-motion timer, so this is normal speed.
    pub time_scale_220: f32,
    /// +224: paused/replay flag.
    pub paused_224: bool,
    /// +236 / +240 / +260: KnownAir time in state, time until landing, jump height.
    pub air_time_236: f32,
    pub air_until_landing_240: f32,
    pub air_jump_height_260: f32,
    /// +333: left push foot planted, `P && !338 && 337`.
    pub push_left_333: bool,
    /// +334: right push foot planted, `P && 338`.
    pub push_right_334: bool,
    /// +335: rising edge of a push-foot plant, `P && !prev334 && !prev333`. Retail's rattle trigger.
    pub push_plant_335: bool,
    /// +336: brake foot planted (State52).
    pub brake_336: bool,
    /// +337: push event present (State56).
    pub push_event_337: bool,
    /// +338: push event on the right toe (State57).
    pub push_right_toe_338: bool,
    /// +339: ManualBrake (State54).
    pub manual_brake_339: bool,
    /// +340: balance non-zero (State60).
    pub balance_340: bool,
    /// +676: bail (State59).
    pub bail_676: bool,
    /// +690: revert (State66).
    pub revert_690: bool,
    /// +716: walking (physical state 500).
    pub walking_716: bool,
    /// +724 / +725: right and left foot down levels.
    pub foot_down_right_724: bool,
    pub foot_down_left_725: bool,
    /// +796: `AudibleFootStepStrength`.
    pub footstep_strength_796: f32,
}

impl AudioState {
    /// One bridge pass. The +335 edge reads +333/+334 from the previous pass before they are
    /// rewritten, exactly as `sub_824B0DA8` orders its stores.
    pub(crate) fn update(&mut self, inputs: &RetailAudioInputs, physical_state: u32) {
        self.com_velocity_prev_112 = self.com_velocity_96;
        self.com_velocity_96 = inputs.com_velocity;
        self.com_velocity_delta_128 =
            std::array::from_fn(|i| self.com_velocity_96[i] - self.com_velocity_prev_112[i]);
        self.wheel_count_200 = inputs.wheel_count & 7;
        self.ground_speed_208 = inputs.ground_speed;
        self.com_speed_prev_216 = self.com_speed_212;
        self.com_speed_212 = length(inputs.com_velocity);
        self.time_scale_220 = 1.0;
        self.paused_224 = false;
        self.air_time_236 = inputs.air_time_in_state;
        self.air_until_landing_240 = inputs.air_time_until_landing;
        self.air_jump_height_260 = inputs.air_jump_height;

        let planted = state_flag(inputs, 55);
        self.push_event_337 = state_flag(inputs, 56);
        self.push_right_toe_338 = state_flag(inputs, 57);
        self.push_plant_335 = planted && !self.push_right_334 && !self.push_left_333;
        self.push_right_334 = planted && self.push_right_toe_338;
        self.push_left_333 = planted && !self.push_right_toe_338 && self.push_event_337;
        self.brake_336 = state_flag(inputs, 52);
        self.manual_brake_339 = state_flag(inputs, 54);
        self.balance_340 = state_flag(inputs, 60);
        self.bail_676 = state_flag(inputs, 59);
        self.revert_690 = state_flag(inputs, 66);
        self.walking_716 = physical_state == 500;

        self.foot_down_right_724 = inputs.offboard_feet[1]
            || inputs.footplant[1]
            || self.push_right_334
            || self.brake_336;
        self.foot_down_left_725 =
            inputs.offboard_feet[0] || inputs.footplant[0] || self.push_left_333;
        self.footstep_strength_796 = inputs.footstep_strength;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs_with(flags: &[usize]) -> RetailAudioInputs {
        let mut inputs = RetailAudioInputs::default();
        for &offset in flags {
            inputs.state_flags[offset - 52] = true;
        }
        inputs
    }

    #[test]
    fn push_plant_edge_fires_once_per_plant_and_selects_the_foot() {
        let mut state = AudioState::default();
        let mut observe = |flags: &[usize]| {
            state.update(&inputs_with(flags), 100);
            (state.push_plant_335, state.push_left_333, state.push_right_334)
        };
        // Left-toe push: event present, planted.
        assert_eq!(observe(&[56, 55]), (true, true, false));
        // Still planted: no second edge.
        assert_eq!(observe(&[56, 55]), (false, true, false));
        // Lifted, then a right-toe plant.
        assert_eq!(observe(&[]), (false, false, false));
        assert_eq!(observe(&[56, 57, 55]), (true, false, true));
    }
}
