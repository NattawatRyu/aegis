//! G4 input-rate guard
//! STOPS: speedhack / action-spam via more than one input per player per tick
//! HOW:   record the last tick each player was accepted; a second input in the
//!        same tick is rejected. The sim folds at most one input per tick, so
//!        flooding buys the attacker nothing.
//! EDGE:  first input in a tick accepted; 2nd/3rd in the SAME tick rejected;
//!        the next tick accepts again.

use super::{ClientInput, GuardCtx, GuardVerdict, InputGuard, RejectReason};
use std::collections::HashMap;

#[derive(Default)]
pub struct InputRateGuard {
    /// player -> last tick for which an input was accepted
    last_tick: HashMap<u8, u32>,
}

impl InputRateGuard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputGuard for InputRateGuard {
    fn name(&self) -> &'static str {
        "input_rate"
    }

    fn forget(&mut self, player: u8) {
        self.last_tick.remove(&player);
    }

    fn check(&mut self, ctx: &GuardCtx, _input: &mut ClientInput) -> GuardVerdict {
        match self.last_tick.get(&ctx.player) {
            Some(&t) if t == ctx.tick => GuardVerdict::Rejected(RejectReason::RateExceeded),
            _ => {
                self.last_tick.insert(ctx.player, ctx.tick);
                GuardVerdict::Ok { anomaly: false }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::Vec2;

    fn input() -> ClientInput {
        ClientInput { seq: 1, tick: 1, move_dir: Vec2::ZERO, aim: Vec2::new(1.0, 0.0), shoot: false }
    }

    #[test]
    fn one_per_tick_accepted_extras_rejected() {
        let mut g = InputRateGuard::new();
        let ctx = GuardCtx { tick: 7, player: 1 };
        assert_eq!(g.check(&ctx, &mut input()), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&ctx, &mut input()), GuardVerdict::Rejected(RejectReason::RateExceeded));
        assert_eq!(g.check(&ctx, &mut input()), GuardVerdict::Rejected(RejectReason::RateExceeded));
    }

    #[test]
    fn next_tick_accepts_again() {
        let mut g = InputRateGuard::new();
        assert_eq!(g.check(&GuardCtx { tick: 7, player: 1 }, &mut input()), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&GuardCtx { tick: 8, player: 1 }, &mut input()), GuardVerdict::Ok { anomaly: false });
    }

    #[test]
    fn different_players_are_independent() {
        let mut g = InputRateGuard::new();
        assert_eq!(g.check(&GuardCtx { tick: 7, player: 1 }, &mut input()), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&GuardCtx { tick: 7, player: 2 }, &mut input()), GuardVerdict::Ok { anomaly: false });
    }
}
