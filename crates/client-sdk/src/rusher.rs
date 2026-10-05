//! RusherBot — the aggressive honest player.
//!
//! Same human hand as [`HonestBot`] (same aim error, seeded), but it plays
//! close: whenever an enemy is in sight it runs straight at it, firing; with
//! nobody in sight it walks like the honest bot. Up close the target fills
//! more of the aim cone, so it hits far more often than a player who keeps
//! walking — without its aim being any better.
//!
//! It exists for the detector's false-positive bound. Hit rate depends on
//! range as much as on aim, and an honest population that never closes in
//! sets the accuracy line from the wrong tail: the esp bot — which, culled,
//! is exactly this player — once tripped that line on 26 of its first 30
//! shots. Honest lobbies mix walkers and rushers so the line is measured
//! against both.

use super::{honest::HonestBot, my_pos, nearest_enemy, unit_towards, Bot, BotCtx};
use aegis_protocol::ClientMsg;

pub struct RusherBot {
    honest: HonestBot,
}

impl RusherBot {
    /// Same hand as `HonestBot::with_seed(seed)`.
    pub fn with_seed(seed: u32) -> Self {
        Self { honest: HonestBot::with_seed(seed) }
    }
}

impl Bot for RusherBot {
    fn name(&self) -> &'static str {
        "rusher"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        if let (Some(me), Some(e), ClientMsg::Input { move_dir, .. }) = (my_pos(ctx), nearest_enemy(ctx), &mut out[0]) {
            *move_dir = unit_towards(me, e.pos);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{PlayerState, Vec2};

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    fn input(b: &mut impl Bot, snap: &[PlayerState]) -> (Vec2, Vec2, bool) {
        match b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: snap })[0] {
            ClientMsg::Input { move_dir, aim, shoot, .. } => (move_dir, aim, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// Runs at what it sees, aiming exactly as the honest bot with the same
    /// seed would — the only difference is where it goes.
    #[test]
    fn charges_the_nearest_enemy_with_the_honest_hand() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(0.0, -10.0)), state(3, Vec2::new(30.0, 0.0))];
        let (move_dir, aim, shoot) = input(&mut RusherBot::with_seed(7), &snap);
        assert_eq!(move_dir, Vec2::new(0.0, -1.0));
        assert!(shoot);
        assert_eq!(aim, input(&mut HonestBot::with_seed(7), &snap).1);
    }

    /// Nobody in sight: it walks like the honest bot instead of standing.
    #[test]
    fn with_nobody_in_sight_it_walks() {
        let snap = [state(1, Vec2::ZERO)];
        let r = input(&mut RusherBot::with_seed(7), &snap);
        assert_eq!(r, input(&mut HonestBot::with_seed(7), &snap));
        assert_ne!(r.0, Vec2::ZERO);
    }
}
