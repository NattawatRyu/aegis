//! AimTrigger — the triggerbot as it is sold: a human aims, the machine
//! pulls the trigger.
//!
//! It walks like an honest player and turns its aim towards the nearest
//! enemy the way a hand does — at most [`TURN_RAD`] a tick, never quite
//! steady ([`HAND_RAD`]). It fires only on the ticks the aim ray passes
//! within [`TRIGGER_RADIUS`] of that enemy. So it is never exact (aim_exact
//! sees nothing), and how fast it fires is how fast the hand got there
//! (reaction sees a person, mostly). What it cannot hide is that it does not
//! miss: every shot it takes is on the target, however small the target
//! looks — the far_aim detector's case.
//!
//! [`super::triggerbot`] is the other kind: it fires the tick an enemy shows,
//! with an honest hand's aim, and is caught by reaction. Found in the Godot
//! demo (2026-10-10): this one was missed by every detector until far_aim.

use super::{honest::HonestBot, jitter, my_pos, nearest_enemy, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// Most the hand turns in one tick, in radians (6 rad/s at 30 Hz).
pub const TURN_RAD: f32 = 0.2;

/// Shake of the hand each tick, either side, in radians.
pub const HAND_RAD: f32 = 0.04;

/// Fires when the aim ray passes this close to the enemy's centre, in world
/// units: 0.8 of the lab's hitbox radius (1.0), so the shot is surely on.
pub const TRIGGER_RADIUS: f32 = 0.8;

pub struct AimTrigger {
    honest: HonestBot,
    /// Where the crosshair points, in radians.
    aim: f32,
    rng: u32,
}

impl AimTrigger {
    pub fn new() -> Self {
        Self::with_seed(0xC2B2_AE35)
    }

    /// Walks like `HonestBot::with_seed(seed)`; the hand has its own stream.
    pub fn with_seed(seed: u32) -> Self {
        Self { honest: HonestBot::with_seed(seed), aim: 0.0, rng: (seed ^ 0x2545_F491) | 1 }
    }
}

impl Default for AimTrigger {
    fn default() -> Self {
        Self::new()
    }
}

/// `a` wrapped into (-π, π].
fn wrap(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    a - t * ((a + std::f32::consts::PI) / t).floor()
}

impl Bot for AimTrigger {
    fn name(&self) -> &'static str {
        "aimtrigger"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        let shake = jitter(&mut self.rng) * HAND_RAD;
        let mut fire = false;
        if let (Some(me), Some(e)) = (my_pos(ctx), nearest_enemy(ctx)) {
            let (dx, dy) = (e.pos.x - me.x, e.pos.y - me.y);
            let turn = wrap(dy.atan2(dx) - self.aim).clamp(-TURN_RAD, TURN_RAD);
            self.aim = wrap(self.aim + turn + shake);
            let (s, c) = self.aim.sin_cos();
            let along = dx * c + dy * s;
            let across = (dx * s - dy * c).abs();
            fire = along > 0.0 && across < TRIGGER_RADIUS;
        }
        let aim = Vec2::new(self.aim.cos(), self.aim.sin());
        if let ClientMsg::Input { aim: a, shoot, .. } = &mut out[0] {
            *a = aim;
            *shoot = fire;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    fn input(b: &mut AimTrigger, tick: u32, snap: &[PlayerState]) -> (Vec2, bool) {
        match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: snap })[0] {
            ClientMsg::Input { aim, shoot, .. } => (aim, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// An enemy behind it, 30 units away: the hand needs ~16 ticks to turn
    /// round, holds fire until the ray is on the target, then every shot it
    /// takes is within TRIGGER_RADIUS — and none is dead-on.
    #[test]
    fn turns_like_a_hand_and_fires_only_on_target() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(-30.0, 0.0))];
        let mut b = AimTrigger::new();
        let (mut first, mut shots, mut exact) = (None, 0, 0);
        for tick in 1..=120 {
            let (aim, shoot) = input(&mut b, tick, &snap);
            let across = (aim.y * -30.0).abs();
            if shoot {
                first.get_or_insert(tick);
                shots += 1;
                assert!(aim.x < 0.0 && across < TRIGGER_RADIUS, "tick {tick}: fired off target");
                exact += u32::from(aim.y.abs() < 1e-3);
            }
        }
        let first = first.expect("never fired");
        assert!(first >= 14, "on target after {first} ticks: no hand turns that fast");
        assert!(shots > 50 && exact < shots / 10, "{shots} shots, {exact} dead-on");
    }

    #[test]
    fn with_nobody_in_sight_it_holds_fire() {
        assert!(!input(&mut AimTrigger::new(), 1, &[state(1, Vec2::ZERO)]).1);
    }

    #[test]
    fn wrap_stays_in_a_half_turn() {
        for a in [-7.0f32, -3.2, 0.0, 3.2, 7.0] {
            let w = wrap(a);
            assert!(w > -std::f32::consts::PI - 1e-6 && w <= std::f32::consts::PI + 1e-6, "{a} -> {w}");
            assert!((w.sin() - a.sin()).abs() < 1e-4 && (w.cos() - a.cos()).abs() < 1e-4, "{a} -> {w}");
        }
    }
}
