//! AimAssistBot — the aimbot as players describe it in use: the human aims
//! and fires; the cheat only pulls the crosshair onto an enemy that is
//! already close to it and close enough to lock.
//!
//! Reported for the WARDOGS cheat (2026-10-19): it "locks on, but the player
//! does the shooting, and the target has to be in range before it locks".
//! That is the usual "aim FOV + max distance + smoothing" setup, laid over an
//! honest player:
//!
//!   - Hand, walk and trigger: [`HonestBot`]'s, unchanged — its aim error,
//!     its walk, its human reaction before the first shot.
//!   - Lock: when the nearest enemy is within [`LOCK_RANGE`] and the hand's
//!     aim is within [`FOV_RAD`] of it, the crosshair closes [`SMOOTH`] of
//!     the gap from where it was last tick to the bearing (plus a humanizer
//!     twist of [`JITTER_RAD`]) instead of going where the hand put it.
//!
//! So everything it does past [`LOCK_RANGE`] is an honest player's, and the
//! far shots far_aim judges are the hand's. Up close the aim stops behaving
//! like a hand's: the gap collapses to almost nothing and sticks to the
//! target as it moves.

use super::{honest::HonestBot, jitter, my_pos, nearest_enemy, Bot, BotCtx};
use crate::humanized::JITTER_RAD;
use aegis_protocol::{ClientMsg, Vec2};

/// Half-angle of the cone around the crosshair the lock works in (~8.6°),
/// the "aim FOV" setting. As wide as the honest hand's largest error, so
/// in range the lock always has the enemy in its cone.
pub const FOV_RAD: f32 = 0.15;

/// Farthest enemy the lock takes, in world units: the cheat's max-distance
/// setting. Under 25 (where `far_aim::FAR_RAD` starts for a hitbox of 1), so
/// every far shot is the hand's.
pub const LOCK_RANGE: f32 = 20.0;

/// Share of the remaining gap the lock closes each tick ("smoothing").
pub const SMOOTH: f32 = 0.5;

pub struct AimAssistBot {
    honest: HonestBot,
    /// How far off the bearing the crosshair was last tick, in radians, if
    /// the lock held it. `None`: the hand had it, and the lock starts from
    /// the hand's error. Kept off the bearing, not as an angle, so the lock
    /// tracks a moving target as the hand does and only shrinks its error.
    held: Option<f32>,
    rng: u32,
}

impl AimAssistBot {
    pub fn new() -> Self {
        Self::with_seed(0x6C8E_9CF5)
    }

    /// Plays like `HonestBot::with_seed(seed)` until the lock takes over;
    /// the humanizer has its own stream.
    pub fn with_seed(seed: u32) -> Self {
        Self { honest: HonestBot::with_seed(seed), held: None, rng: (seed ^ 0x2545_F491) | 1 }
    }
}

impl Default for AimAssistBot {
    fn default() -> Self {
        Self::new()
    }
}

/// `a` wrapped into (-π, π].
fn wrap(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    a - t * ((a + std::f32::consts::PI) / t).floor()
}

/// Is an enemy at `dist` whose bearing is `gap` off the hand's aim inside
/// the lock?
pub fn locks(dist: f32, gap: f32) -> bool {
    dist <= LOCK_RANGE && gap.abs() <= FOV_RAD
}

impl Bot for AimAssistBot {
    fn name(&self) -> &'static str {
        "aimassist"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        let twist = jitter(&mut self.rng) * JITTER_RAD;
        if let ClientMsg::Input { aim, .. } = &mut out[0] {
            let hand = aim.y.atan2(aim.x);
            let (mut now, mut held) = (hand, None);
            if let (Some(me), Some(e)) = (my_pos(ctx), nearest_enemy(ctx)) {
                let (dx, dy) = (e.pos.x - me.x, e.pos.y - me.y);
                let bearing = dy.atan2(dx);
                let off = wrap(hand - bearing);
                if locks(dx.hypot(dy), off) {
                    let from = self.held.unwrap_or(off);
                    let err = from + SMOOTH * (twist - from);
                    (now, held) = (wrap(bearing + err), Some(err));
                }
            }
            self.held = held;
            *aim = Vec2::new(now.cos(), now.sin());
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

    /// Each tick's gap between aim and bearing, and whether it fired, over
    /// `ticks` ticks facing an enemy at `to`.
    fn run(b: &mut impl Bot, to: Vec2, ticks: u32) -> Vec<(f32, bool)> {
        let snap = [state(1, Vec2::ZERO), state(2, to)];
        (1..=ticks)
            .map(|tick| match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
                ClientMsg::Input { aim, shoot, .. } => (wrap(to.y.atan2(to.x) - aim.y.atan2(aim.x)).abs(), shoot),
                _ => panic!("expected Input"),
            })
            .collect()
    }

    /// In range the lock holds the aim within the humanizer's twist, where
    /// the honest hand it rides on is off by up to 0.15.
    #[test]
    fn in_range_the_lock_holds_the_aim() {
        let g = run(&mut AimAssistBot::new(), Vec2::new(0.0, 10.0), 100);
        assert!(g[20..].iter().all(|&(x, _)| x <= JITTER_RAD + 1e-4), "lock let go: {g:?}");
        let hand = run(&mut HonestBot::new(), Vec2::new(0.0, 10.0), 100);
        assert!(hand[20..].iter().filter(|&&(x, _)| x > JITTER_RAD).count() > 40, "the hand is already that good");
    }

    /// Out of range it is the honest player exactly: same aim, same trigger.
    #[test]
    fn out_of_range_it_is_the_honest_player() {
        let to = Vec2::new(0.0, 40.0);
        let (a, h) = (run(&mut AimAssistBot::with_seed(7), to, 200), run(&mut HonestBot::with_seed(7), to, 200));
        for (t, (x, y)) in a.iter().zip(&h).enumerate() {
            assert!((x.0 - y.0).abs() < 1e-5 && x.1 == y.1, "tick {}: {x:?} vs honest {y:?}", t + 1);
        }
    }

    /// The human pulls the trigger, after a human reaction.
    #[test]
    fn fires_after_a_human_reaction() {
        use crate::honest::{REACT_MAX, REACT_MIN};
        let g = run(&mut AimAssistBot::new(), Vec2::new(0.0, 10.0), 100);
        let first = g.iter().position(|&(_, s)| s).expect("never fired") as u32 + 1;
        assert!((1 + REACT_MIN..=1 + REACT_MAX).contains(&first), "first shot on tick {first}");
    }
}
