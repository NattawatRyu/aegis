//! WardogsBot — the commercial package: ESP + aimbot + humanizer, with a
//! human reaction delay.
//!
//! Modelled on what a Thai cheat shop sells for WARDOGS (2026-10-19): "ESP
//! through walls" plus "aimbot that locks on players", per-user randomised
//! builds and a kernel driver to read game memory. None of that tooling
//! matters to the server; what reaches it is the inputs. So this bot is the
//! strongest client the package can make, inside the rules:
//!
//!   - ESP: walks straight at the nearest enemy it was sent ([`super::esp`]).
//!   - Aimbot with humanizer: aim locked on that enemy, twisted by at most
//!     [`JITTER_RAD`] so no shot is dead-on ([`super::humanized`]).
//!   - Reaction delay: waits a human reaction ([`Reflex`]) before the first
//!     shot at each enemy newly in sight, so reaction sees a person.
//!
//! Culling leaves the ESP half nothing to draw, so what is left to catch is
//! a humanized aimbot that reacts like a human: it still does not miss
//! targets that look small — far_aim's case.

use super::{jitter, my_pos, nearest_enemy, rotate, unit_towards, Bot, BotCtx};
use crate::honest::Reflex;
use crate::humanized::JITTER_RAD;
use aegis_protocol::{ClientMsg, Vec2};

pub struct WardogsBot {
    seq: u32,
    rng: u32,
    reflex: Reflex,
}

impl WardogsBot {
    pub fn new() -> Self {
        Self::with_seed(0x1B87_3593)
    }

    /// A different aim twist and reaction sequence per seed. 0 is remapped.
    pub fn with_seed(seed: u32) -> Self {
        let rng = if seed == 0 { 0x1B87_3593 } else { seed };
        Self { seq: 0, rng, reflex: Reflex::with_seed(rng ^ 0x5BD1_E995) }
    }
}

impl Default for WardogsBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for WardogsBot {
    fn name(&self) -> &'static str {
        "wardogs"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let me = my_pos(ctx).unwrap_or(Vec2::ZERO);
        let ready = self.reflex.nearest_ready(ctx);
        let (move_dir, aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => {
                let to = unit_towards(me, e.pos);
                (to, rotate(to, jitter(&mut self.rng) * JITTER_RAD), ready)
            }
            None => (Vec2::ZERO, Vec2::new(1.0, 0.0), false),
        };
        vec![ClientMsg::Input {
            seq: self.seq,
            tick: ctx.tick,
            proof: ctx.proof,
            move_dir,
            aim,
            shoot,
            offset: 0,
            older: Vec::new(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::honest::{REACT_MAX, REACT_MIN};
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    /// Chases what it was sent, aims within the humanizer's twist, and
    /// holds fire for a human reaction.
    #[test]
    fn chases_locks_and_waits_a_human_reaction() {
        let mut b = WardogsBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(0.0, -10.0)), state(3, Vec2::new(30.0, 0.0))];
        let mut first = None;
        for tick in 1..100 {
            match b.act(&BotCtx { tick, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
                ClientMsg::Input { move_dir, aim, shoot, .. } => {
                    assert_eq!(move_dir, Vec2::new(0.0, -1.0));
                    let off = (aim.x.atan2(-aim.y)).abs();
                    assert!(off <= JITTER_RAD + 1e-6, "tick {tick}: aim {off} off");
                    if shoot && first.is_none() {
                        first = Some(tick);
                    }
                }
                _ => panic!("expected Input"),
            }
        }
        let first = first.expect("never fired");
        assert!((1 + REACT_MIN..=1 + REACT_MAX).contains(&first), "first shot on tick {first}");
    }

    /// Culled to itself, it has nothing to chase or shoot.
    #[test]
    fn with_nothing_sent_it_does_nothing() {
        let mut b = WardogsBot::new();
        let snap = [state(1, Vec2::ZERO)];
        match b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &snap })[0] {
            ClientMsg::Input { move_dir, shoot, .. } => {
                assert_eq!(move_dir, Vec2::ZERO);
                assert!(!shoot);
            }
            _ => panic!("expected Input"),
        }
    }
}
