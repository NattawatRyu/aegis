//! G6 move-speed guard
//! STOPS: speedhack via move_dir magnitude (sending (10,0) to move 10x/tick)
//! HOW:   clamp move_dir to unit length before the sim consumes it
//! EDGE:  (10,0) -> len 1.0 (flagged anomaly) | exactly-unit untouched |
//!        sub-unit untouched (no anomaly) | one ulp over unit (a normalized
//!        vector's rounding) -> clamped, not flagged
//!
//! Honest clients never send a move vector longer than 1, so a clamp actually
//! changing the vector is itself the anomaly signal — flagged, not blocked, so
//! a legitimate float rounding edge never kicks a real player.

use super::{ClientInput, GuardCtx, GuardVerdict, InputGuard};

/// How far past unit length a move vector may be before the clamp counts as
/// an anomaly. f32 normalization overshoots by ~1e-7; a speedhack worth
/// running asks for far more than 0.01%.
pub const ROUNDING_TOLERANCE: f32 = 1e-4;

pub struct MoveSpeedGuard;

impl InputGuard for MoveSpeedGuard {
    fn name(&self) -> &'static str {
        "move_speed"
    }

    fn check(&mut self, _ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict {
        let len = input.move_dir.len();
        input.move_dir = input.move_dir.clamped_unit();
        GuardVerdict::Ok { anomaly: len > 1.0 + ROUNDING_TOLERANCE }
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

    /// Edge: a client that normalizes its direction can land one f32 ulp over
    /// unit length. That is clamped (no speed gained) but is not an anomaly;
    /// anything past the tolerance still is.
    #[test]
    fn rounding_over_unit_is_clamped_not_flagged() {
        let mut g = MoveSpeedGuard;
        let ulp_over = Vec2::new(1.0 + f32::EPSILON, 0.0);
        assert!(ulp_over.len() > 1.0);
        let mut i = with_move(ulp_over);
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: false });
        assert!(i.move_dir.len() <= 1.0);

        let mut i = with_move(Vec2::new(1.0 + 2.0 * ROUNDING_TOLERANCE, 0.0));
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: true });
        assert!(i.move_dir.len() <= 1.0);
    }

    #[test]
    fn sub_unit_move_untouched_no_anomaly() {
        let mut g = MoveSpeedGuard;
        let mut i = with_move(Vec2::new(0.3, 0.4));
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: false });
        assert_eq!(i.move_dir, Vec2::new(0.3, 0.4));
    }
}
