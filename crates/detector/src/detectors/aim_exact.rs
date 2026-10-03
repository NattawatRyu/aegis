//! D2 aim-exact detector
//! CATCHES: snap aimbots — aim computed from the snapshot lands on the exact
//!          bearing to the target, shot after shot
//! SIGNAL:  share of shots whose aim_err < EXACT_RAD
//! EDGE:    shots < MIN_SHOTS -> no verdict | aim_err == EXACT_RAD is not
//!          exact | share == THRESHOLD -> no flag | just above -> flag
//!
//! Independent of hit rate: an aimbot that snaps exactly but shoots at a
//! target out of range misses, and still shows up here. A human's error is
//! spread over a cone; landing within EXACT_RAD of dead-on is rare for one
//! shot and implausible for most of them.
//!
//! Blind spot, by design: an aimbot that adds jitter wider than EXACT_RAD
//! escapes this detector entirely. The accuracy detector is what catches it.

use crate::{Detector, Flag, FlagReason, PlayerStats};

pub const MIN_SHOTS: u32 = 30;

/// A shot this close to the true bearing counts as "exact" (~0.06°). Far
/// above f32 noise on the server's angle (~1e-6 rad), far below any human
/// hand.
pub const EXACT_RAD: f32 = 1e-3;

/// Flag strictly above this share of exact shots.
///
/// Measured (`aegis-harness sweep 250`, 1000 honest bots, 2026-10-02, walled
/// arena + culled snapshots): p50 0.006, p99 0.025, max 0.040. Snap aimbot:
/// 0.99. (Open arena, 2026-09-30: max 0.037.) At 0.25 the line is
/// ~7x the best honest run and still catches an aimbot switched on for only
/// a third of its shots (a "toggle" cheater).
pub const THRESHOLD: f32 = 0.25;

pub struct AimExactDetector;

impl Detector for AimExactDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::AimExact
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        let shots = s.shots();
        if shots < MIN_SHOTS {
            return None;
        }
        let exact = s.aim_errs.iter().filter(|&&e| e < EXACT_RAD).count();
        let share = exact as f32 / shots as f32;
        (share > THRESHOLD).then_some(Flag {
            player: s.player,
            reason: FlagReason::AimExact,
            value: share,
            threshold: THRESHOLD,
            samples: shots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `exact` shots dead-on, the rest `off` radians away.
    fn player(exact: usize, rest: usize, off: f32) -> PlayerStats {
        let mut aim_errs = vec![0.0; exact];
        aim_errs.extend(std::iter::repeat_n(off, rest));
        PlayerStats { player: 3, aim_errs, ..Default::default() }
    }

    #[test]
    fn too_few_shots_is_no_verdict_even_if_all_exact() {
        assert_eq!(AimExactDetector.check(&player(MIN_SHOTS as usize - 1, 0, 0.0)), None);
    }

    #[test]
    fn all_exact_at_min_shots_is_flagged() {
        let f = AimExactDetector.check(&player(MIN_SHOTS as usize, 0, 0.0)).expect("flag");
        assert_eq!((f.reason, f.value, f.samples), (FlagReason::AimExact, 1.0, MIN_SHOTS));
    }

    #[test]
    fn exact_rad_itself_does_not_count_as_exact() {
        assert_eq!(AimExactDetector.check(&player(0, 100, EXACT_RAD)), None);
        assert!(AimExactDetector.check(&player(0, 100, EXACT_RAD * 0.999)).is_some());
    }

    #[test]
    fn threshold_itself_is_not_flagged_just_above_is() {
        let at = (THRESHOLD * 100.0).round() as usize;
        assert_eq!(AimExactDetector.check(&player(at, 100 - at, 0.1)), None);
        assert!(AimExactDetector.check(&player(at + 1, 99 - at, 0.1)).is_some());
    }

    #[test]
    fn perfect_hits_with_jitter_are_invisible_here() {
        // The documented blind spot: 100% "accuracy" but never exact.
        let s = PlayerStats { player: 3, hits: 100, aim_errs: vec![0.02; 100], ..Default::default() };
        assert_eq!(AimExactDetector.check(&s), None);
    }
}
