//! G3 sanity guard
//! STOPS: NaN / infinite / non-finite vectors that would corrupt the sim
//!        (a NaN aim silently breaks hitscan; a NaN move poisons position)
//! HOW:   reject any input whose move_dir or aim has a non-finite component
//! EDGE:  finite vectors (incl. zero and large) pass; NaN or +/-inf rejected

use super::{ClientInput, GuardCtx, GuardVerdict, InputGuard, RejectReason};
use aegis_protocol::Vec2;

pub struct SanityGuard;

fn finite(v: Vec2) -> bool {
    v.x.is_finite() && v.y.is_finite()
}

impl InputGuard for SanityGuard {
    fn name(&self) -> &'static str {
        "sanity"
    }

    fn check(&mut self, _ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict {
        if finite(input.move_dir) && finite(input.aim) {
            GuardVerdict::Ok { anomaly: false }
        } else {
            GuardVerdict::Rejected(RejectReason::MalformedInput)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ClientInput {
        ClientInput { seq: 1, tick: 1, move_dir: Vec2::ZERO, aim: Vec2::new(1.0, 0.0), shoot: false }
    }

    #[test]
    fn finite_input_passes() {
        let mut g = SanityGuard;
        let mut i = ClientInput { move_dir: Vec2::new(1e6, -3.0), ..base() };
        assert_eq!(g.check(&GuardCtx { tick: 1, player: 1 }, &mut i), GuardVerdict::Ok { anomaly: false });
    }

    #[test]
    fn nan_move_rejected() {
        let mut g = SanityGuard;
        let mut i = ClientInput { move_dir: Vec2::new(f32::NAN, 0.0), ..base() };
        assert_eq!(
            g.check(&GuardCtx { tick: 1, player: 1 }, &mut i),
            GuardVerdict::Rejected(RejectReason::MalformedInput)
        );
    }

    #[test]
    fn infinite_aim_rejected() {
        let mut g = SanityGuard;
        let mut i = ClientInput { aim: Vec2::new(f32::INFINITY, 0.0), ..base() };
        assert_eq!(
            g.check(&GuardCtx { tick: 1, player: 1 }, &mut i),
            GuardVerdict::Rejected(RejectReason::MalformedInput)
        );
    }
}
