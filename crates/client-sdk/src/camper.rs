//! CamperBot — the honest player who holds a spot.
//!
//! Same human hand as [`HonestBot`] (same aim error, seeded), but it never
//! moves: it holds its spawn and shoots whatever comes into sight.
//!
//! It is in the honest population for what it does to everyone else's hit
//! rate: a target that stands still is easy, and a rusher who closes on one
//! hits far more than against a moving one. The esp bot, which culled is just
//! a rusher, hit 26 of its first 30 shots in the standard scenario — where
//! several players stand still — and under 0.6 in lobbies where nobody did.
//! Without campers the accuracy line is measured against an easier world than
//! a real one.

use super::{honest::HonestBot, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct CamperBot {
    honest: HonestBot,
}

impl CamperBot {
    /// Same hand as `HonestBot::with_seed(seed)`.
    pub fn with_seed(seed: u32) -> Self {
        Self { honest: HonestBot::with_seed(seed) }
    }
}

impl Bot for CamperBot {
    fn name(&self) -> &'static str {
        "camper"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        if let ClientMsg::Input { move_dir, .. } = &mut out[0] {
            *move_dir = Vec2::ZERO;
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

    fn input(b: &mut impl Bot, snap: &[PlayerState]) -> (Vec2, Vec2, bool) {
        match b.act(&BotCtx { tick: 1, my_id: 1, token: 0, snapshot: snap })[0] {
            ClientMsg::Input { move_dir, aim, shoot, .. } => (move_dir, aim, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// Never moves; aims and fires exactly as the honest bot with its seed.
    #[test]
    fn holds_still_with_the_honest_hand() {
        for snap in [vec![state(1, Vec2::ZERO)], vec![state(1, Vec2::ZERO), state(2, Vec2::new(10.0, 0.0))]] {
            let (move_dir, aim, shoot) = input(&mut CamperBot::with_seed(7), &snap);
            let (_, h_aim, h_shoot) = input(&mut HonestBot::with_seed(7), &snap);
            assert_eq!((move_dir, aim, shoot), (Vec2::ZERO, h_aim, h_shoot));
        }
    }
}
