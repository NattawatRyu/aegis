//! G5 replay guard
//! STOPS: replay / seq reuse — resending a captured old input, or reordering
//! HOW:   require the per-player `seq` to be strictly increasing; a seq <= the
//!        last accepted one is a stale/replayed packet and is rejected.
//! EDGE:  first seq (any value) accepted; equal seq rejected; lower seq
//!        rejected; strictly higher seq accepted.

use super::{ClientInput, GuardCtx, GuardVerdict, InputGuard, RejectReason};
use std::collections::HashMap;

#[derive(Default)]
pub struct ReplayGuard {
    /// player -> highest seq accepted so far
    last_seq: HashMap<u8, u32>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputGuard for ReplayGuard {
    fn name(&self) -> &'static str {
        "replay"
    }

    fn forget(&mut self, player: u8) {
        self.last_seq.remove(&player);
    }

    fn check(&mut self, ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict {
        match self.last_seq.get(&ctx.player) {
            Some(&s) if input.seq <= s => GuardVerdict::Rejected(RejectReason::Replay),
            _ => {
                self.last_seq.insert(ctx.player, input.seq);
                GuardVerdict::Ok { anomaly: false }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::Vec2;

    fn input_seq(seq: u32) -> ClientInput {
        ClientInput { seq, tick: 1, move_dir: Vec2::ZERO, aim: Vec2::new(1.0, 0.0), shoot: false }
    }

    #[test]
    fn strictly_increasing_seq_accepted() {
        let mut g = ReplayGuard::new();
        let ctx = GuardCtx { tick: 1, player: 1 };
        assert_eq!(g.check(&ctx, &mut input_seq(1)), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&ctx, &mut input_seq(2)), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&ctx, &mut input_seq(3)), GuardVerdict::Ok { anomaly: false });
    }

    #[test]
    fn replayed_equal_seq_rejected() {
        let mut g = ReplayGuard::new();
        let ctx = GuardCtx { tick: 1, player: 1 };
        assert_eq!(g.check(&ctx, &mut input_seq(5)), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&ctx, &mut input_seq(5)), GuardVerdict::Rejected(RejectReason::Replay));
    }

    #[test]
    fn out_of_order_lower_seq_rejected() {
        let mut g = ReplayGuard::new();
        let ctx = GuardCtx { tick: 1, player: 1 };
        assert_eq!(g.check(&ctx, &mut input_seq(10)), GuardVerdict::Ok { anomaly: false });
        assert_eq!(g.check(&ctx, &mut input_seq(4)), GuardVerdict::Rejected(RejectReason::Replay));
    }
}
