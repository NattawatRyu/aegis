//! G6 move-speed guard
//! STOPS: speedhack via move_dir magnitude (sending (10,0) to move 10x/tick)
//! HOW:   clamp move_dir to unit length before the sim consumes it
//! EDGE:  (10,0) -> len 1.0 (flagged anomaly) | exactly-unit untouched |
//!        sub-unit untouched (no anomaly)
//!
//! Honest clients never send a move vector longer than 1, so a clamp actually
//! changing the vector is itself the anomaly signal — flagged, not blocked, so
//! a legitimate float rounding edge never kicks a real player.

use super::{ClientInput, GuardCtx, GuardVerdict, InputGuard};

pub struct MoveSpeedGuard;

impl InputGuard for MoveSpeedGuard {
    fn name(&self) -> &'static str {
        "move_speed"
    }

    fn check(&mut self, _ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict {
        let before = input.move_dir;
        input.move_dir = input.move_dir.clamped_unit();
        GuardVerdict::Ok { anomaly: input.move_dir != before }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::Vec2;

    fn with_move(move_dir: Vec2) -> ClientInput {
        ClientInput { seq: 1, tick: 1, move_dir, aim: Vec2::new(1.0, 0.0), shoot: false }
    }

    #[test]
    fn oversized_move_is_clamped_and_flagged() {
        let mut g = MoveSpeedGuard;
        let mut i = with_move(Vec2::new(10.0, 0.0));
        let v = g.check(&GuardCtx { tick: 1, player: 1 }, &mut i);
        assert_eq!(v, GuardVerdict::Ok { anomaly: true });
        assert!((i.move_dir.len() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unit_move_untouched_no_anomaly() {
        let mut g = MoveSpeedGuard;
        let mut i = with_move(Vec2::new(1.0, 0.0));
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: false });
        assert_eq!(i.move_dir, Vec2::new(1.0, 0.0));
    }

    #[test]
    fn sub_unit_move_untouched_no_anomaly() {
        let mut g = MoveSpeedGuard;
        let mut i = with_move(Vec2::new(0.3, 0.4));
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: false });
        assert_eq!(i.move_dir, Vec2::new(0.3, 0.4));
    }
}
