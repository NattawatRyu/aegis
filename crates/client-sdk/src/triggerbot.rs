//! TriggerBot — the cheat that is only fast.
//!
//! An honest walker in every respect — same walk, same human hand
//! ([`HonestBot`], [`AIM_ERROR_RAD`](crate::honest::AIM_ERROR_RAD)) — except
//! that it skips the reaction: it fires the tick an enemy is in sight. Its
//! aim says nothing, its hit rate is a walker's, every input is legal. The
//! only evidence it leaves is `Shot::react` = 0, so it is the bot that shows
//! whether a reaction-time detector catches anything aim does not (the aimbot
//! and humanized aimbot are instant too, but they also snap).
//!
//! A real triggerbot fires when the player's own crosshair crosses an enemy,
//! so its reaction includes the human's time to aim there and is not 0. In
//! the lab aim lands on the enemy at once, so "fire on sight" stands in for
//! it: the cleanest case of the signal, stronger than a live game would show.

use super::{honest::HonestBot, nearest_enemy, Bot, BotCtx};
use aegis_protocol::ClientMsg;

pub struct TriggerBot {
    honest: HonestBot,
}

impl TriggerBot {
    pub fn new() -> Self {
        Self::with_seed(0x27D4_EB2F)
    }

    /// Same hand and walk as `HonestBot::with_seed(seed)`.
    pub fn with_seed(seed: u32) -> Self {
        Self { honest: HonestBot::with_seed(seed) }
    }
}

impl Default for TriggerBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for TriggerBot {
    fn name(&self) -> &'static str {
        "triggerbot"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.honest.act(ctx);
        if let ClientMsg::Input { shoot, .. } = &mut out[0] {
            *shoot = nearest_enemy(ctx).is_some();
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

    fn input(b: &mut impl Bot, tick: u32, snap: &[PlayerState]) -> (Vec2, Vec2, bool) {
        match b.act(&BotCtx { tick, my_id: 1, token: 0, snapshot: snap })[0] {
            ClientMsg::Input { move_dir, aim, shoot, .. } => (move_dir, aim, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// The tick an enemy appears it fires; the honest bot with the same seed
    /// holds fire. Everything else — walk, aim — is the honest bot's.
    #[test]
    fn fires_on_sight_with_the_honest_hand() {
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(0.0, -10.0))];
        let (t, h) = (&mut TriggerBot::with_seed(7), &mut HonestBot::with_seed(7));
        for tick in 1..=crate::honest::REACT_MAX + 1 {
            let (tm, ta, ts) = input(t, tick, &snap);
            let (hm, ha, _) = input(h, tick, &snap);
            assert_eq!((tm, ta), (hm, ha), "tick {tick}");
            assert!(ts, "held fire on tick {tick}");
        }
        let (_, _, first) = input(&mut HonestBot::with_seed(7), 1, &snap);
        assert!(!first, "the honest bot it is compared with fires on sight too");
    }

    /// Nobody in sight: nothing to fire at.
    #[test]
    fn with_nobody_in_sight_it_holds_fire() {
        let snap = [state(1, Vec2::ZERO)];
        assert!(!input(&mut TriggerBot::new(), 1, &snap).2);
    }
}
