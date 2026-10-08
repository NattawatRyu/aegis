//! EspBot — the wallhack.
//!
//! ESP draws every enemy the client was sent, walls or not, and the player
//! uses it to walk straight at the nearest one and pre-aim the corner it will
//! come round. That is all this bot does: it heads for the nearest enemy in
//! its snapshot and aims at it with a human hand (the honest bot's error), so
//! its aim says nothing the detector could catch. It reacts like the honest
//! bot too ([`Reflex`]): ESP could let a player pre-aim and fire sooner, but
//! culled it has nothing to pre-aim at, so it is an honest rusher in every
//! respect — the proxy the harness uses it as.
//!
//! No guard rejects it and no detector should flag it: everything it sends is
//! legal and its aim is human. What stops it is that the snapshot it reads is
//! the server's per-player view — an enemy behind a wall is not in it — so
//! there is nothing behind the wall to draw. The harness counts what it was
//! sent that it could not have seen (`hidden`); culling holds iff that is 0.

use super::{jitter, my_pos, nearest_enemy, rotate, unit_towards, Bot, BotCtx};
use crate::honest::{Reflex, AIM_ERROR_RAD};
use aegis_protocol::{ClientMsg, Vec2};

pub struct EspBot {
    seq: u32,
    rng: u32,
    reflex: Reflex,
}

impl EspBot {
    pub fn new() -> Self {
        Self { seq: 0, rng: 0xC2B2_AE35, reflex: Reflex::with_seed(0xC2B2_AE35 ^ 0x5BD1_E995) }
    }
}

impl Default for EspBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for EspBot {
    fn name(&self) -> &'static str {
        "esp"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let me = my_pos(ctx).unwrap_or(Vec2::ZERO);
        let ready = self.reflex.nearest_ready(ctx);
        let (move_dir, aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => {
                let to = unit_towards(me, e.pos);
                (to, rotate(to, jitter(&mut self.rng) * AIM_ERROR_RAD), ready)
            }
            None => (Vec2::ZERO, Vec2::new(1.0, 0.0), false),
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, move_dir, aim, shoot }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    /// It walks at whatever it was sent. Given an enemy (say, one behind a
    /// wall in an uncull'd snapshot) it heads straight for it.
    #[test]
    fn heads_for_the_nearest_enemy_it_was_sent() {
        let mut b = EspBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(0.0, -10.0)), state(3, Vec2::new(30.0, 0.0))];
        match b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &snap })[0] {
            ClientMsg::Input { move_dir, shoot, .. } => {
                assert_eq!(move_dir, Vec2::new(0.0, -1.0));
                assert!(!shoot, "fired before a human reaction");
            }
            _ => panic!("expected Input"),
        }
    }

    /// Its reaction is the honest one: nothing before `REACT_MIN`, and
    /// firing by `REACT_MAX` ticks after the enemy came into sight.
    #[test]
    fn reacts_like_an_honest_player() {
        use crate::honest::{REACT_MAX, REACT_MIN};
        let mut b = EspBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(0.0, -10.0))];
        let first = (1..100)
            .find(|&tick| {
                matches!(
                    b.act(&BotCtx { tick, my_id: 1, token: 0, snapshot: &snap })[0],
                    ClientMsg::Input { shoot: true, .. }
                )
            })
            .expect("never fired");
        assert!((1 + REACT_MIN..=1 + REACT_MAX).contains(&first), "first shot on tick {first}");
    }

    /// Culled to itself, it has nothing to chase and stands still.
    #[test]
    fn with_nothing_sent_it_has_nothing_to_chase() {
        let mut b = EspBot::new();
        let snap = [state(1, Vec2::ZERO)];
        match b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: &snap })[0] {
            ClientMsg::Input { move_dir, shoot, .. } => {
                assert_eq!(move_dir, Vec2::ZERO);
                assert!(!shoot);
            }
            _ => panic!("expected Input"),
        }
    }
}
