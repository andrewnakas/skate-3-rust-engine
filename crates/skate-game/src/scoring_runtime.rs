//! ScoreModule integration. The simulation owns recognition and accounting;
//! the APT movie only consumes the resulting publication.
use crate::{apt_vm::Value, hud_runtime};
use skate_core::{
    animation::output::attributes::AttributeName,
    physics::filtered_state::FilteredCategory,
    scoring::{
        carrier::{Carrier, delay_ticks},
        catalog,
        conversions::LINKS,
        session::Session,
    },
};
use skate_data::{collections::Collections, scoring::ScoringData};

/// `air_horizontal_distance`. The five air metrics are consecutive from here.
const AIR_METRIC_BASE: usize = 129;

/// The localisation ids `sub_825E51A0` carries as string literals at 8220E238..8220E494.
///
/// They live in the executable, not in the vault -- searching the owner's collections for
/// `ID_TRICK_AIR*` finds nothing -- so a port has to hold them as literals too. The HUD's
/// own language table does resolve them: `Air`, `FS`, `BS`, `Frontflip`, `Backflip`.
mod trick_tokens {
    /// 8220E3AC. The bare fallback when no record has been announced.
    pub const AIR: &str = "ID_TRICK_AIR";
    /// 8220E254 / its BS sibling.
    pub const FS_SPIN: &str = "ID_TRICK_AIR_FS_SPIN";
    pub const BS_SPIN: &str = "ID_TRICK_AIR_BS_SPIN";
    /// 8220E474 and 8220E454, appended by the sign of the flip direction.
    pub const FRONTFLIP: &str = "ID_TRICK_AIR_METRICS_FRONTFLIP";
    pub const BACKFLIP: &str = "ID_TRICK_AIR_METRICS_BACKFLIP";
}

/// Compose the displayed trick name the way `sub_825E51A0` does.
///
/// The result is one `#`, then space-separated localisation ids and a bare degree count,
/// which [`crate::apt_text`] resolves token by token. Retail formats the head with
/// `"#%s %d "` (8220E43C) or `"#%s "` (8220E444) and then `strcat`s the flip suffix on.
///
/// The port previously published only the announced record's own label, with nothing
/// appended, which is why a body flip displayed as a bare air.
///
/// Not ported: the Cab / Half-Cab tokens (gated at 0x825E508C on fakie-and-not-switch) and
/// the nollie record swap through the metadata link column, both of which only rename an
/// already-named trick; and the Miracle Whip special case for scorable 252.
fn compose_trick_name(
    base: Option<&str>,
    spin_turns: i32,
    flip_direction: i32,
    switch: bool,
    goofy: bool,
) -> String {
    // 0x825E53C4: a spin's side is the parity of switch, the spin's own sign and stance.
    let backside = switch ^ (spin_turns < 0) ^ goofy;
    let mut name = String::from("#");
    name.push_str(match base {
        Some(label) => label,
        None if spin_turns == 0 => trick_tokens::AIR,
        None if backside => trick_tokens::BS_SPIN,
        None => trick_tokens::FS_SPIN,
    });
    // 82DA8BE0 keeps the rotation as signed half-turns; the display prints degrees.
    if spin_turns != 0 {
        name.push(' ');
        name.push_str(&(spin_turns.abs() * 180).to_string());
    }
    // 0x825E57A8: the suffix is chosen by the sign of the air collector's +2348, and a
    // zero -- no flip, or a flip with no grab to record its side -- appends nothing.
    match flip_direction {
        d if d > 0 => {
            name.push(' ');
            name.push_str(trick_tokens::FRONTFLIP);
        }
        d if d < 0 => {
            name.push(' ');
            name.push_str(trick_tokens::BACKFLIP);
        }
        _ => {}
    }
    name
}

/// One of the four run accumulators the gap/context collector keeps, `82DA7B50`.
///
/// Each measures how far the skater travelled horizontally while its own condition held,
/// across as many separate runs as the air contains. Retail also keeps the longest single
/// run at +24 and a run count at +28; neither reaches the score.
#[derive(Clone, Copy, Default)]
struct ContextRun {
    /// +0, the position the current run started from.
    start: [f32; 3],
    /// +16, the distance covered by the run in progress.
    current: f32,
    /// +20, the distance of every run already closed.
    banked: f32,
    /// byte +32.
    running: bool,
}

impl ContextRun {
    fn advance(&mut self, position: [f32; 3], active: bool) {
        if active && !self.running {
            self.current = 0.0;
            self.running = true;
            self.start = position;
        }
        if !self.running {
            return;
        }
        // The lane mask before the length drops Y, as the air's own distance metric does.
        let (dx, dz) = (position[0] - self.start[0], position[2] - self.start[2]);
        let distance = dx.hypot(dz);
        if active {
            self.current = distance;
            return;
        }
        self.banked += distance;
        self.current = 0.0;
        self.running = false;
    }

    /// `82DA88E8` sums the closed runs and the one in progress before the curve.
    fn total(self) -> f32 {
        self.banked + self.current
    }
}

/// The curves `82DA88E8` evaluates, in the order `82DA89A0` drives the runs.
///
/// `addi r10,r11,1440` / `1280` / `1360` / `1200` against the collector tuning block.
const CONTEXT_CURVES: [u16; 4] = [0x5a0, 0x500, 0x550, 0x4b0];

/// `air_metric`, the scorable `82DA8550` banks the gap/context total under.
const CONTEXT_METRIC: usize = 237;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Collector {
    None,
    Ground,
    Air,
    Grind,
    Offboard,
    Special,
}
pub(crate) struct Frame {
    pub tick: u32,
    pub dt: f32,
    pub category: FilteredCategory,
    pub state: u32,
    pub descriptor: Option<AttributeName>,
    pub grind_id: i32,
    pub flags: u32,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub forward: [f32; 3],
    pub switch: bool,
    pub fakie: bool,
    pub nollie: bool,
    pub body_flip: bool,
    /// Air 445. Selects the flip's direction; see [`Runtime::flip_direction`].
    pub body_flip_side: bool,
    /// PhysOut.Ground +192: the hips line test's ray origin, the hips part's world
    /// position. `82DB6EC0` publishes it unconditionally.
    pub hips_position: [f32; 3],
    /// PhysOut.Ground +208, and `None` when Ground +320 says the test missed. The hips
    /// test's hit position, republished only while `player+1524` is set.
    pub hips_ground: Option<[f32; 3]>,
    /// PhysOut.Ground +260: the packed surface tag the hips test hit.
    pub hips_surface: u32,
    pub suspend_air: bool,
    pub landing: skate_core::animation::landing_quality::Output,
    pub teleported: bool,
    pub reverting: bool,
}
pub(crate) struct Runtime {
    pub data: ScoringData,
    pub session: Session,
    collector: Collector,
    carriers: [Option<Carrier>; 4],
    held: [f32; 4],
    distance: [f32; 4],
    metric_rewards: [f32; 4],
    metric_started: [bool; 4],
    start: [f32; 3],
    previous: [f32; 3],
    previous_heading: f32,
    spin: f32,
    peak: f32,
    air_factor: f32,
    air_repetition: f32,
    air_repetition_set: bool,
    grab_chain: u32,
    air_metrics: [f32; 5],
    /// 82DA8BE0's +2344: the air's rotation quantised to whole half-turns and signed by
    /// its direction. The reward uses only its magnitude; the sign is for the trick name.
    pub spin_turns: i32,
    /// 82DA8EB8's +2348: `0` until a body flip is credited, then `+1` or `-1` for its
    /// side. Retail writes it only while an announced grab carrier is current, so an
    /// ungrabbed flip earns no flip reward -- but it is still *named* as a flip.
    pub flip_direction: i32,
    /// The gap/context collector's four run accumulators, at collector +192/+240/+288/+336.
    context_runs: [ContextRun; 4],
    /// The hips ray origin's height at take-off.
    ///
    /// `82DA7DA0` is the air collector's Reset (vtable 823281A4 slot 2) and takes a whole
    /// ground query into collector+160 with no reference, once, at Enter. The per-frame
    /// query then measures its drop against *the lower of* that snapshot and the present
    /// hips height, so a rise after take-off cannot inflate the gap.
    takeoff_hips_height: f32,
    /// 82DA93D8's one-shot latch at +777: the class-3 bonus has already been paid for
    /// this air, and cannot be paid again however many flip tricks follow.
    flip_bonus_paid: bool,
    /// 82DA8EB8's +2396 latch, which is also scoring output byte 14651: a body flip
    /// happened during this air, whatever the carrier was.
    ///
    /// This is *not* what names a flip -- that is [`Self::flip_direction`], by way of the
    /// output block's +12316. Byte 14651 only fires one HUD event: `82775328` reads it at
    /// 0x82775560 and calls the backend's vtable+24 with `r4 = 17`. Nothing consumes it
    /// here yet, because that event is not ported.
    pub flip_seen: bool,
    landing_countdown: u32,
    idle_ticks: u32,
    collector_ticks: u32,
    manual_revert_ticks: u32,
    revert_id: Option<usize>,
    sequence_active: bool,
    sequence_score: f32,
    /// The composed name the display shows, rebuilt each tick by [`compose_trick_name`].
    trick_name: String,
    /// The announced record's own localisation id, before spin and flip are added.
    base_trick_label: Option<String>,
    stance: [bool; 4],
    clean: bool,
    sketchy: bool,
    pub new_trick: bool,
    pub modified_trick: bool,
    pub close_tricks: bool,
    /// The multiplier this runtime last reported, `52(r31)` in 82666BC0.
    published_multiplier: f32,
    /// Set for the one tick on which the published sequence multiplier changed, to the value
    /// it changed to. 82666BC0 polls `140(r30)` against its own cached copy and reacts to any
    /// change in either direction; the ScoreModule itself plays nothing.
    pub multiplier_changed: Option<f32>,
}
impl Runtime {
    pub fn load(data: &Collections) -> Result<Self, String> {
        Ok(Self {
            data: ScoringData::load(data)?,
            session: Session::default(),
            collector: Collector::None,
            carriers: std::array::from_fn(|_| None),
            held: [0.; 4],
            distance: [0.; 4],
            metric_rewards: [0.; 4],
            metric_started: [false; 4],
            start: [0.; 3],
            previous: [0.; 3],
            previous_heading: 0.,
            spin: 0.,
            peak: 0.,
            air_factor: 1.,
            air_repetition: 1.,
            air_repetition_set: false,
            grab_chain: 0,
            air_metrics: [0.; 5],
            spin_turns: 0,
            flip_direction: 0,
            context_runs: [ContextRun::default(); 4],
            takeoff_hips_height: 0.,
            flip_bonus_paid: false,
            flip_seen: false,
            landing_countdown: 0,
            idle_ticks: 0,
            collector_ticks: 0,
            manual_revert_ticks: 0,
            revert_id: None,
            sequence_active: false,
            sequence_score: 0.,
            trick_name: String::new(),
            base_trick_label: None,
            stance: [false; 4],
            clean: false,
            sketchy: false,
            new_trick: false,
            modified_trick: false,
            close_tricks: false,
            published_multiplier: 1.,
            multiplier_changed: None,
        })
    }
    pub fn hud_input(&self) -> hud_runtime::Input {
        hud_runtime::Input {
            sequence_score: self.sequence_score as i32,
            line_score: self.session.holder.snapshot.line as i32,
            sequence_timer: (self.session.line.points / self.data.line_drain) as i32,
            line_time: self.session.line.points / self.data.line_drain,
            line_capacity: self.data.line_capacity,
            multiplier: self.session.combo.multiplier,
            clean: self.clean,
            sketchy: self.sketchy,
            stance: self.stance,
            trick_name: self.trick_name.clone(),
            // Tricks.GetCurrentTrickMetrics, sub_825C27C0: the composed name, then the
            // module's switch (M+20), fakie (M+158), "clear the display" (M+156) and
            // "this is a new trick rather than a modification" (M+157) bytes.
            trick_metrics: [
                Value::Text(self.trick_name.clone()),
                Value::Bool(self.stance[0]),
                Value::Bool(self.stance[1]),
                Value::Bool(self.close_tricks),
                Value::Bool(self.new_trick),
            ],
            context_tricks: Vec::new(),
        }
    }

    /// Uses the same landing-quality thresholds as the retail scoring/HUD path without requiring
    /// audio to infer clean or sketchy from animation names.
    pub(crate) fn classify_landing_audio(
        &self,
        landing: skate_core::animation::landing_quality::Output,
    ) -> (bool, bool) {
        if !landing.landing_data_167 {
            return (false, false);
        }
        let clean = landing.landing_type_96 == 0;
        let sketchy = landing.landing_type_96 == 1
            && landing.sideways_speed_84 > self.data.sketchy_side_speed
            && landing.spin_92.abs() > 0.5;
        (clean, sketchy)
    }
    fn penalty(&self, id: usize) -> f32 {
        let Some(d) = self.data.by_id(id) else {
            return 1.;
        };
        if d.metadata.repetition_applies() {
            self.data.repetition.evaluate(
                self.session
                    .holder
                    .repetition_count(d.metadata)
                    .unwrap_or(0) as f32,
            )
        } else {
            1.
        }
    }
    fn finish(&mut self, slot: usize, complete: bool, keep_metric: bool) {
        if let Some(mut carrier) = self.carriers[slot].take() {
            if complete {
                carrier.complete(self.data.unannounced_factor);
                if self.collector == Collector::Air {
                    self.session
                        .holder
                        .end_trick(carrier.scorable, carrier.reward);
                } else {
                    let metric = self.collector == Collector::Grind
                        || self.collector == Collector::Ground && slot < 3;
                    if !(keep_metric && self.collector == Collector::Ground) {
                        let reward = if keep_metric {
                            0.
                        } else if metric {
                            if carrier.scorable.score_type == 9 {
                                carrier.reward.max(self.metric_rewards[slot])
                            } else {
                                self.metric_rewards[slot].max(0.)
                            }
                        } else {
                            carrier.reward
                        };
                        self.session.holder.credit_trick(carrier.scorable, reward);
                    }
                }
            }
        }
        if !keep_metric {
            self.metric_started[slot] = false;
            self.held[slot] = 0.;
            self.distance[slot] = 0.;
            self.metric_rewards[slot] = 0.;
        }
    }
    fn carrier(&mut self, slot: usize, id: Option<usize>, f: &Frame) -> Result<(), String> {
        if self.carriers[slot].as_ref().map(|c| c.scorable.id) != id {
            let conversion = id.filter(|new| {
                self.collector == Collector::Air
                    && self.carriers[slot].is_some()
                    && LINKS[*new].0 >= 0
            });
            let chained_grab = self.collector == Collector::Air
                && id
                    .and_then(|id| self.data.by_id(id))
                    .is_some_and(|d| d.metadata.class == 2)
                && self.carriers[slot]
                    .as_ref()
                    .is_some_and(|c| c.scorable.class == 2 && !c.announced);
            let previous_tick = self.carriers[slot].as_ref().map(|c| c.start_tick);
            let keep_metric = id.is_some()
                && (self.collector == Collector::Grind
                    || self.collector == Collector::Ground && slot == 0);
            if conversion.is_none() {
                if chained_grab {
                    self.carriers[slot] = None;
                    self.grab_chain += 1;
                } else {
                    self.finish(slot, true, keep_metric);
                    self.grab_chain = 0;
                }
            }
            if let Some(id) = id {
                let d = self
                    .data
                    .by_id(id)
                    .ok_or_else(|| format!("Missing native scorable {id}"))?;
                let factor = self.penalty(id)
                    * if self.collector == Collector::Air {
                        self.air_factor
                    } else {
                        1.
                    };
                let mut carrier = Carrier::new(
                    d.metadata,
                    d.points,
                    factor,
                    self.data.announcement.evaluate(d.points as f32),
                    if chained_grab {
                        previous_tick.unwrap_or(f.tick)
                    } else {
                        f.tick
                    },
                    delay_ticks(
                        d.completion_delay,
                        if self.grab_chain > 1 {
                            self.data.collector.scalar(0x698)
                        } else {
                            0.
                        },
                    ),
                    f.switch,
                    f.fakie,
                );
                if let Some(old) = self.carriers[slot].as_mut() {
                    old.convert_to(&mut carrier, self.data.unannounced_factor);
                    self.modified_trick = true;
                }
                if conversion.is_some() {
                    self.base_trick_label = Some(d.label.clone());
                }
                self.carriers[slot] = Some(carrier);
                self.sequence_active = true;
                self.idle_ticks = 0;
            }
        }
        let penalty = self.carriers[slot]
            .as_ref()
            .map(|c| self.penalty(c.scorable.id))
            .unwrap_or(1.);
        if let Some(c) = self.carriers[slot].as_mut() {
            if c.announce(f.tick, self.data.unannounced_factor) {
                let d = self
                    .data
                    .by_id(c.scorable.id)
                    .ok_or("Missing announced scorable")?;
                self.base_trick_label = Some(d.label.clone());
                self.stance = [f.switch, f.fakie, f.nollie, false];
                self.new_trick = true;
                if self.collector == Collector::Air && !self.air_repetition_set {
                    self.air_repetition = penalty;
                    self.air_repetition_set = true;
                }
            }
            let distance_metric = self.collector == Collector::Grind
                || self.collector == Collector::Ground && slot < 3;
            let was_active = self.metric_started[slot];
            self.metric_started[slot] = true;
            if c.announced && !(self.collector == Collector::Air && f.suspend_air) {
                // 82DB0588 starts a distance collector at time/distance zero.
                // Every active frame still updates its reference position.
                if !distance_metric || was_active {
                    self.held[slot] += f.dt;
                }
                let previous_distance = self.distance[slot];
                let displacement =
                    std::array::from_fn::<_, 3, _>(|i| f.position[i] - self.previous[i]);
                let delta = if self.collector == Collector::Ground && slot == 1 {
                    displacement
                        .iter()
                        .zip(f.forward)
                        .map(|(x, y)| x * y)
                        .sum::<f32>()
                } else {
                    (displacement.iter().map(|x| x * x).sum::<f32>()).sqrt()
                };
                if was_active && delta > f32::from_bits(0x3ba3d70a) {
                    self.distance[slot] += delta;
                }
                let curve = match self.collector {
                    Collector::Air if c.scorable.class == 2 => Some(0x460),
                    Collector::Offboard => Some(0x140),
                    _ => None,
                };
                if let Some(curve) = curve {
                    c.reward += (self.data.collector.curve(curve, self.held[slot]) * f.dt)
                        * c.announcement_threshold;
                }
                let curve = match (self.collector, c.scorable.score_type) {
                    (Collector::Ground, 9) => Some((0x000, 0x050)),
                    (Collector::Ground, 8) => Some((0x0f0, 0x0a0)),
                    (Collector::Ground, _) if slot == 2 => Some((0x230, 0x1e0)),
                    (Collector::Grind, _) => Some((0x2d0, 0x280)),
                    _ => None,
                };
                if let Some((distance, time)) = curve {
                    let delta = (self.data.collector.curve(distance, self.distance[slot])
                        - self.data.collector.curve(distance, previous_distance))
                    .max(0.);
                    self.metric_rewards[slot] = (self
                        .data
                        .collector
                        .curve(time, self.held[slot])
                        .mul_add(f.dt, delta))
                    .mul_add(c.announcement_threshold, self.metric_rewards[slot]);
                }
            }
        }
        Ok(())
    }
    /// The gap/context collector, `82DA89A0` driving four `82DA7B50` run accumulators.
    ///
    /// This is retail's "cleared a big gap" score, and it was missing entirely. The ground
    /// query `82DA7A38` reports, from the hips test alone:
    ///
    /// ```text
    /// out[0..16) = Ground[192]                  ; the hips ray origin, unconditional
    /// if (byte[Ground+320] == 0) return;        ; a miss leaves every field zero
    /// out+16 = Ground[192].y - Ground[208].y
    /// out+20 = min(Ground[192].y, reference.y) - Ground[208].y
    /// out+25 = (tag & 0x7F) == 10
    /// out+26 = ((tag >> 7) & 31) == 8
    /// ```
    ///
    /// and `82DA89A0` gates one run on each of `out+25`, `out+26`, `out+20 > 0x680` (3 m)
    /// and `out+20 > 0x67C` (6 m). Each run measures horizontal distance travelled while
    /// its gate holds, and `82DA88E8` puts the total through a curve: up to 900 for the
    /// first three and **1800** for the 6 m one.
    ///
    /// Not ported: `82DA89A0` additionally requires `collector+185 == 0` for the first run.
    /// That byte is unidentified, so the run is gated on the surface test alone.
    fn advance_context_runs(&mut self, f: &Frame) {
        let tag = f.hips_surface;
        let gates = match f.hips_ground {
            None => [false; 4],
            Some(contact) => {
                let reference = self.takeoff_hips_height.min(f.hips_position[1]);
                let drop = reference - contact[1];
                [
                    tag & 0x7f == 10,
                    (tag >> 7) & 0x1f == 8,
                    drop > self.data.collector.scalar(0x680),
                    drop > self.data.collector.scalar(0x67c),
                ]
            }
        };
        for (run, active) in self.context_runs.iter_mut().zip(gates) {
            run.advance(f.hips_position, active);
        }
    }

    /// `82DA88E8`: the four runs through their curves. Banked as scorable 237 at the
    /// landing, and shown live in the sequence total meanwhile.
    fn context_reward(&self) -> f32 {
        self.context_runs
            .iter()
            .zip(CONTEXT_CURVES)
            .map(|(run, curve)| self.data.collector.curve(curve, run.total()))
            .sum()
    }

    /// 82DA93D8's one-shot class-3 bonus: `reward += 0x600 * points * factor`.
    ///
    /// ```text
    /// cmpwi cr6,r6,3          ; the current carrier is a flip trick
    /// lbz   r11,777(r30)      ; and the bonus has not been paid this air
    /// bl    0x82dac780        ; run the geometric test, which sets output byte 14648
    /// lbz   r9,14648(r10)
    /// lwz   r11,588(r30)      ; = carrier+4, the authored points
    /// lfs   f9,1536(r10)      ; tuning 0x600 = 4.0
    /// fmuls f8,f9,f10 / fmadds f7,f8,f13,f0 / stfs f7,176(r31)
    /// ```
    ///
    /// The authored scalar is 4.0, so this is worth four times the trick's own points --
    /// +400 on a kickflip, +1000 on a laserflip -- and it was missing entirely.
    ///
    /// `82DAC780`'s test asks whether the hips' ground contact lies *between* the deck and
    /// the hips: the vectors from the contact to each must point opposite ways, and
    /// neither may be degenerate.
    ///
    /// ```text
    /// lbz   r11,320(r10) ; beqlr        ; the hips test must have hit something
    /// v63 = block[144] - block[208]     ; contact -> deck
    /// v62 = block[192] - block[208]     ; contact -> hips
    /// vcmpgtfp dot3(v63,v63), 0.01      ; both non-degenerate
    /// vcmpgtfp dot3(v62,v62), 0.01
    /// vcmpgtfp 0.0, dot3(v63,v62)       ; and opposed
    /// stb   r10,14648(r11)
    /// ```
    fn pay_flip_bonus(&mut self, f: &Frame) {
        if self.flip_bonus_paid {
            return;
        }
        let Some(carrier) = self.carriers[0].as_ref() else {
            return;
        };
        // 82DA93D8 reads the carrier's presence byte +124, not its announcement: an
        // unannounced carrier has not credited its points yet, and the bonus goes onto the
        // same reward those points will land in.
        if carrier.scorable.class != 3 {
            return;
        }
        let Some(contact) = f.hips_ground else {
            return;
        };
        let to_deck: [f32; 3] = std::array::from_fn(|i| f.position[i] - contact[i]);
        let to_hips: [f32; 3] = std::array::from_fn(|i| f.hips_position[i] - contact[i]);
        let dot = |a: &[f32; 3], b: &[f32; 3]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        // 820D71E8 = 0.01, compared against the squared lengths rather than the lengths.
        let epsilon = f32::from_bits(0x3C23_D70A);
        if !(dot(&to_deck, &to_deck) > epsilon && dot(&to_hips, &to_hips) > epsilon) {
            return;
        }
        if !(dot(&to_deck, &to_hips) < 0.0) {
            return;
        }
        let bonus = (self.data.collector.scalar(0x600) * carrier.points as f32) * carrier.factor;
        if let Some(carrier) = self.carriers[0].as_mut() {
            carrier.reward += bonus;
        }
        self.flip_bonus_paid = true;
    }

    pub fn advance(&mut self, f: Frame) -> Result<(), String> {
        self.new_trick = false;
        self.modified_trick = false;
        self.close_tricks = false;
        let descriptor = f
            .descriptor
            .and_then(|name| self.data.by_name(name))
            .filter(|d| !(f.flags & 0x02000000 != 0 && [67, 68, 69].contains(&d.metadata.id)))
            .map(|d| (d.metadata.id, d.metadata.class, d.metadata.score_type));
        let mut next = match f.category {
            FilteredCategory::Ground => Collector::Ground,
            FilteredCategory::Air => Collector::Air,
            FilteredCategory::Grind => Collector::Grind,
            FilteredCategory::Offboard | FilteredCategory::OffboardAir => Collector::Offboard,
            _ => Collector::None,
        };
        if f.state == 600 && matches!(next, Collector::Ground | Collector::Air) {
            next = Collector::Special;
        }
        if next == Collector::Air
            && self.collector == Collector::Ground
            && descriptor.is_some_and(|d| d.1 == 0)
        {
            next = Collector::Ground;
        }
        if next == Collector::Ground
            && self.collector == Collector::Air
            && descriptor.is_some_and(|d| d.1 == 3)
        {
            next = Collector::Air;
        }
        if f.teleported {
            next = Collector::None;
        }
        if next != self.collector {
            let complete = next != Collector::None;
            let previous_type = self
                .carriers
                .iter()
                .flatten()
                .last()
                .map(|c| c.scorable.score_type);
            for slot in 0..4 {
                self.finish(slot, complete, false);
            }
            if self.collector == Collector::Air {
                if complete {
                    // 82DA8550 banks these by bare id (`li r4,129` / `bl 0x82da6260`).
                    // They have no authored record, so resolving them through the
                    // authored table returned nothing and every air metric reward --
                    // distance, peak height, height gain, spin and flip -- was
                    // discarded at the landing that was supposed to bank it.
                    for (i, reward) in self.air_metrics.into_iter().enumerate() {
                        if let Some(metric) = catalog::metadata(AIR_METRIC_BASE + i) {
                            self.session.holder.end_trick(metric, reward);
                        }
                    }
                    // 82DA8550 closes the gap runs and banks their total the same way:
                    // `bl 0x82da88e8` / `li r4,237` / `bl 0x82da6260`.
                    if let Some(metric) = catalog::metadata(CONTEXT_METRIC) {
                        self.session
                            .holder
                            .end_trick(metric, self.context_reward());
                    }
                }
                self.session.holder.finish_collector();
                self.landing_countdown = 2;
            }
            self.collector = next;
            self.start = f.position;
            self.peak = f.position[1];
            self.spin = 0.;
            self.previous_heading = f.forward[0].atan2(f.forward[2]);
            self.air_metrics = [0.; 5];
            self.spin_turns = 0;
            self.flip_direction = 0;
            self.context_runs = [ContextRun::default(); 4];
            self.takeoff_hips_height = f.hips_position[1];
            self.flip_bonus_paid = false;
            self.flip_seen = false;
            self.air_repetition = 1.;
            self.air_repetition_set = false;
            self.air_factor = 1.;
            self.grab_chain = 0;
            self.collector_ticks = 0;
            self.manual_revert_ticks = 0;
            self.revert_id = None;
            if next == Collector::Air {
                self.session.holder.reward_sequence(1.);
                self.landing_countdown = 0;
                if previous_type == Some(8) {
                    self.air_factor *= self.data.collector.scalar(0x68c);
                }
                if previous_type == Some(5) {
                    self.air_factor *= self.data.collector.scalar(0x690);
                }
                if f.velocity[0] * f.velocity[0] + f.velocity[2] * f.velocity[2]
                    < self.data.collector.scalar(0x66c).powi(2)
                {
                    self.air_factor *= self.data.collector.scalar(0x688);
                }
                if f.switch && !f.fakie {
                    self.air_factor *= self.data.collector.scalar(0x684);
                }
                if f.fakie && !f.switch {
                    self.air_factor *= self.data.collector.scalar(0x694);
                }
            }
        }
        let mut ids = [None; 4];
        if !(self.collector == Collector::Air && f.suspend_air) {
            self.collector_ticks = self.collector_ticks.saturating_add(1);
        }
        match self.collector {
            Collector::Ground => {
                ids[0] = if f.flags & 0x80000000 != 0 {
                    Some(0)
                } else if f.flags & 0x40000000 != 0 {
                    Some(1)
                } else {
                    None
                };
                ids[1] = if f.flags & 0x08000000 != 0 {
                    Some(2)
                } else if f.flags & 0x04000000 != 0 {
                    Some(3)
                } else {
                    None
                };
                if ids[1].is_some() && f.reverting {
                    self.manual_revert_ticks += 1;
                    ids[1] = None;
                } else {
                    self.manual_revert_ticks = 0;
                }
                if self.manual_revert_ticks > 6 {
                    ids[1] = None;
                }
                if ids[1].is_some() {
                    if let Some(c) = &self.carriers[1] {
                        ids[1] = Some(c.scorable.id);
                    }
                }
                ids[2] = descriptor
                    .filter(|d| d.0 == 60 && f.flags & 0x02000000 != 0)
                    .map(|d| d.0);
                if f.flags & 0x20000000 != 0 {
                    self.revert_id = Some(4);
                } else if f.flags & 0x10000000 != 0 {
                    self.revert_id = Some(5);
                }
                ids[3] = if f.reverting { self.revert_id } else { None };
            }
            Collector::Air => ids[0] = descriptor.filter(|d| d.2 != 5).map(|d| d.0),
            Collector::Grind => ids[0] = (f.grind_id >= 0).then_some(f.grind_id as usize),
            Collector::Offboard => ids[0] = descriptor.filter(|d| d.2 == 6).map(|d| d.0),
            Collector::Special => ids[0] = descriptor.filter(|d| d.0 == 234).map(|d| d.0),
            Collector::None => {}
        }
        for (slot, id) in ids.into_iter().enumerate() {
            self.carrier(slot, id, &f)?;
        }
        if self.collector == Collector::Air && !f.suspend_air {
            if self.collector_ticks > 5 || f.flags & 0x01000000 != 0 {
                self.sequence_active = true;
            }
            let heading = f.forward[0].atan2(f.forward[2]);
            let delta = (heading - self.previous_heading + std::f32::consts::PI)
                .rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            self.spin += delta;
            self.previous_heading = heading;
            self.peak = self.peak.max(f.position[1]);
            let dx = f.position[0] - self.start[0];
            let dz = f.position[2] - self.start[2];
            let scale = self.air_factor * self.air_repetition;
            self.air_metrics[0] =
                self.data.collector.curve(0x3c0, (dx * dx + dz * dz).sqrt()) * scale;
            self.air_metrics[1] =
                self.data.collector.curve(0x410, self.peak - self.start[1]) * scale;
            self.air_metrics[2] = self
                .data
                .collector
                .curve(0x320, f.position[1] - self.start[1])
                * scale;
            // 82DA8BE0 signs the turn count by the rotation's own direction and keeps it
            // at +2344; the reward takes its magnitude.
            let degrees = self.spin.to_degrees();
            let turns =
                ((degrees.abs() + self.data.collector.scalar(0x63c)) / 180.) as i32;
            self.spin_turns = if degrees < 0. { -turns } else { turns };
            self.air_metrics[3] =
                self.data.collector.curve(0x370, (turns * 180) as f32) * scale;
            // 82DA8EB8. The +2396 latch, and with it scoring output byte 14651, is set on
            // the first flipping frame whatever the current carrier is -- that is what the
            // trick display names a flip from. Only +2348, and so the reward, waits for an
            // announced grab carrier.
            if f.body_flip {
                self.flip_seen = true;
                if self.carriers[0]
                    .as_ref()
                    .is_some_and(|c| c.announced && c.scorable.class == 2)
                {
                    self.flip_direction = if f.body_flip_side { -1 } else { 1 };
                }
            }
            // Recomputed from the latched +2348 every frame, flipping or not, so the
            // reward survives the end of the grab that authorised it.
            self.air_metrics[4] =
                self.flip_direction.abs() as f32 * self.data.collector.scalar(0x640) * scale;
            self.pay_flip_bonus(&f);
            self.advance_context_runs(&f);
        }
        let active = self.carriers.iter().any(Option::is_some) || self.collector == Collector::Air;
        self.idle_ticks = if active {
            0
        } else {
            self.idle_ticks.saturating_add(1)
        };
        if self.session.holder.has_pending_sequence() {
            if f.landing.landing_data_167 {
                self.clean = f.landing.landing_type_96 == 0;
                self.sketchy = f.landing.landing_type_96 == 1
                    && f.landing.sideways_speed_84 > self.data.sketchy_side_speed
                    && f.landing.spin_92.abs() > 0.5;
            }
            if self.landing_countdown == 0 {
                let mut factor = 1.;
                if f.switch && !f.fakie {
                    factor *= self.data.collector.scalar(0x61c);
                }
                if f.fakie && !f.switch {
                    factor *= self.data.collector.scalar(0x62c);
                }
                if self.collector == Collector::Grind {
                    factor *= self.data.collector.scalar(0x628);
                }
                if self.clean {
                    factor *= self.data.collector.scalar(0x630);
                }
                if self.sketchy {
                    factor *= self.data.collector.scalar(0x620);
                }
                self.session.holder.reward_sequence(factor);
            } else {
                self.landing_countdown -= 1;
            }
        }
        let scales = match self.collector {
            Collector::Air => (0x668, 0x664),
            Collector::Grind => (0x618, 0x614),
            Collector::Offboard => (0x608, 0x604),
            _ => (0x610, 0x60c),
        };
        let line_scale = if active {
            self.data.collector.scalar(scales.0)
        } else {
            1.
        };
        let combo_scale = if active {
            self.data.collector.scalar(scales.1)
        } else {
            1.
        };
        self.session
            .line
            .advance(f.dt, self.data.line_drain, line_scale, active);
        self.session
            .combo
            .timer
            .advance(f.dt, self.data.combo_drain, combo_scale, active);
        let bailout = self.collector == Collector::None && self.sequence_active;
        if self.sequence_active && (bailout || self.idle_ticks >= 3 && self.landing_countdown == 0)
        {
            if bailout {
                self.session.holder.cancel_pending();
            }
            self.sequence_score =
                self.session
                    .publish_sequence(&self.data.session_rules(), 1., bailout, true);
            self.sequence_active = false;
            // 82775328 -> 82774E88 closes only for ScoreModule reset/bail
            // output 14630 (82DA4010/82DA4238), not a banked landing.
            self.close_tricks = bailout;
        } else if self.sequence_active {
            let s = &self.session.holder.snapshot;
            self.sequence_score = (s.accumulated
                + s.general_pending
                + s.fingerflip_pending
                + self
                    .carriers
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        c.as_ref().map(|c| {
                            if self.collector == Collector::Grind
                                || self.collector == Collector::Ground && i < 3
                            {
                                if c.scorable.score_type == 9 {
                                    c.reward.max(self.metric_rewards[i])
                                } else {
                                    self.metric_rewards[i]
                                }
                            } else {
                                c.reward
                            }
                        })
                    })
                    .sum::<f32>()
                + self.air_metrics.iter().sum::<f32>()
                + self.context_reward())
                * self.session.combo.multiplier;
        }
        if self.session.line.expired || f.teleported || bailout {
            self.sequence_score = 0.;
        }
        self.session.settle_line(f.teleported || bailout, active);
        self.previous = f.position;
        // The multiplier poll, after settlement so a line that drops it back to x1 is seen as
        // the change it is. The cached copy is updated whether or not anything reacted, which
        // is 82666BC0's unconditional `stfs f31,52(r31)`.
        let multiplier = self.session.combo.multiplier;
        self.multiplier_changed = (multiplier != self.published_multiplier).then_some(multiplier);
        self.published_multiplier = multiplier;
        // The name is rebuilt rather than assigned once, because the spin and the flip that
        // go into it keep accumulating after the record that named the trick was announced.
        // 82666BC0's counterpart recomposes whenever its dirty flag is set, for the same
        // reason. A closed display drops the base label so the next air starts clean.
        if self.close_tricks {
            self.base_trick_label = None;
        }
        self.trick_name = compose_trick_name(
            self.base_trick_label.as_deref(),
            self.spin_turns,
            self.flip_direction,
            f.switch,
            // M+187 is the negation of PlayerStance_IsRegular. This engine publishes no
            // stance, so the parity is that of a regular skater; a goofy skater's spins
            // will read FS for BS until one is published.
            false,
        );
        Ok(())
    }
}

#[cfg(test)]
mod trick_name_tests {
    use super::{compose_trick_name, trick_tokens};

    /// The user-visible symptom this fixes: a body flip was displayed as a bare air,
    /// because nothing ever appended the flip token 82DA8EB8's +2348 selects.
    #[test]
    fn a_body_flip_is_named_and_takes_its_side_from_the_flip_direction() {
        assert_eq!(
            compose_trick_name(None, 0, 1, false, false),
            format!("#{} {}", trick_tokens::AIR, trick_tokens::FRONTFLIP)
        );
        assert_eq!(
            compose_trick_name(None, 0, -1, false, false),
            format!("#{} {}", trick_tokens::AIR, trick_tokens::BACKFLIP)
        );
        // A flip with no grab to record its side leaves +2348 at zero, and retail appends
        // nothing at all rather than guessing a side.
        assert_eq!(
            compose_trick_name(None, 0, 0, false, false),
            format!("#{}", trick_tokens::AIR)
        );
    }

    /// 82DA8BE0 keeps signed half-turns; the display prints whole degrees.
    #[test]
    fn a_spin_prints_degrees_and_picks_its_side_by_parity() {
        assert_eq!(
            compose_trick_name(Some("ID_TRICK_FLIP_KICKFLIP"), 2, 0, false, false),
            "#ID_TRICK_FLIP_KICKFLIP 360"
        );
        assert_eq!(
            compose_trick_name(Some("ID_TRICK_FLIP_KICKFLIP"), -3, 0, false, false),
            "#ID_TRICK_FLIP_KICKFLIP 540"
        );
        // Unnamed spins fall back to the FS/BS tokens, chosen by switch ^ sign ^ goofy.
        for (turns, switch, goofy, expected) in [
            (1, false, false, trick_tokens::FS_SPIN),
            (-1, false, false, trick_tokens::BS_SPIN),
            (1, true, false, trick_tokens::BS_SPIN),
            (1, false, true, trick_tokens::BS_SPIN),
            (-1, true, true, trick_tokens::BS_SPIN),
        ] {
            assert_eq!(
                compose_trick_name(None, turns, 0, switch, goofy),
                format!("#{expected} 180"),
                "turns {turns} switch {switch} goofy {goofy}"
            );
        }
    }

    /// Everything after the single leading `#` has to be tokens the text layer can look
    /// up, so the name must never carry a second `#` or a double space.
    #[test]
    fn the_composed_name_is_one_hash_and_resolvable_tokens() {
        let name = compose_trick_name(Some("ID_TRICK_FLIP_KICKFLIP"), 2, 1, true, false);
        assert_eq!(name.matches('#').count(), 1);
        assert!(name.starts_with('#'));
        assert!(!name.contains("  "));
        assert_eq!(
            name.trim_start_matches('#').split(' ').collect::<Vec<_>>(),
            ["ID_TRICK_FLIP_KICKFLIP", "360", trick_tokens::FRONTFLIP]
        );
    }
}
