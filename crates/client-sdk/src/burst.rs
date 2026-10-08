//! BurstBot — the toggle cheater.
//!
//! Plays as an honest player (same walk, same human hand) and switches a snap
//! aimbot on for a stretch of the match — [`ON`] to [`OFF`] — the way a real
//! cheater binds it to a key and uses it when it matters. While it is on,
//! every shot lands dead on the bearing, exactly as [`super::aimbot`]'s.
//!
//! What it beats is a detector that averages over the whole match: the honest
//! shots before and after dilute the snapped ones below the line. What
//! catches it is the online monitor's window, which only looks at the last
//! `WINDOW` shots — mostly snapped, while the burst is on.

use super::{honest::HonestBot, my_pos, nearest_enemy, unit_towards, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// First tick the aimbot is on.
pub const ON: u32 = 200;
/// First tick it is off again.
pub const OFF: u32 = 250;

pub struct BurstBot {
    /// Its own hand when the aimbot is off.
    honest: HonestBot,
}

impl BurstBot {
    pub fn new() -> Self {
        Self { honest: HonestBot::with_seed(0x27D4_EB2F) }
    }
}

impl Default for BurstBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for BurstBot {
    fn name(&self) -> &'static str {
        "burst"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        if let (Some(e), ClientMsg::Input { aim, shoot, .. }) = (nearest_enemy(ctx), &mut out[0]) {
            // A cheater: fires the instant it has a target, on or off. Only
            // its aim is the honest hand while the aimbot is off.
            *shoot = true;
            if (ON..OFF).contains(&ctx.tick) {
                *aim = unit_towards(my_pos(ctx).unwrap_or(Vec2::ZERO), e.pos);
            }
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

    fn aim_at(b: &mut BurstBot, tick: u32) -> (Vec2, bool) {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(10.0, 0.0))];
        match b.act(&BotCtx { tick, my_id: 1, token: 0, snapshot: &snap })[0] {
            ClientMsg::Input { aim, shoot, .. } => (aim, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// Dead on only inside [ON, OFF); a human hand on either side of it.
    #[test]
    fn snaps_only_while_switched_on() {
        let mut b = BurstBot::new();
        for tick in [1, ON - 1, ON, OFF - 1, OFF, OFF + 30] {
            let exact = aim_at(&mut b, tick).0 == Vec2::new(1.0, 0.0);
            assert_eq!(exact, (ON..OFF).contains(&tick), "tick {tick}");
        }
    }

    /// No human reaction, on or off: it fires the first tick it sees a target.
    #[test]
    fn fires_instantly_on_or_off() {
        assert!(aim_at(&mut BurstBot::new(), ON).1);
        assert!(aim_at(&mut BurstBot::new(), 1).1);
    }
}
