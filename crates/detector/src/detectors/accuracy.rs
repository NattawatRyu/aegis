//! D1 accuracy detector
//! CATCHES: aimbots, including ones that add jitter to hide a perfect snap
//! SIGNAL:  hits / shots over the player's whole stream
//! EDGE:    shots < MIN_SHOTS -> no verdict | hit rate == THRESHOLD -> no flag |
//!          just above -> flag
//!
//! The blunt instrument: no human hits almost everything. It cannot tell a
//! great player from a subtle aimbot near the line — that is aim_exact's job,
//! and why the threshold sits well above the best honest run (see THRESHOLD).

use crate::{Detector, Flag, FlagReason, PlayerStats};

/// Fewer shots than this and hit rate is noise, not evidence.
pub const MIN_SHOTS: u32 = 30;

/// Flag strictly above this hit rate.
///
/// Measured (`aegis-harness sweep 250`, 1000 honest bots, 2026-10-02, walled
/// arena + culled snapshots): p50 0.112, p99 0.199, max 0.231. Both aimbots:
/// 0.96-0.99. (Open arena, 2026-09-30: max 0.438.) The line sits ~1.8x the
/// open-arena best honest run, because a real player population has a fatter
/// top tail than one seeded bot — re-measure on real telemetry before
/// trusting it on a live game.
pub const THRESHOLD: f32 = 0.8;

pub struct AccuracyDetector;

impl Detector for AccuracyDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::Accuracy
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        let shots = s.shots();
        if shots < MIN_SHOTS {
            return None;
        }
        let rate = s.hits as f32 / shots as f32;
        (rate > THRESHOLD).then_some(Flag {
            player: s.player,
            reason: FlagReason::Accuracy,
            value: rate,
            threshold: THRESHOLD,
            samples: shots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(shots: u32, hits: u32) -> PlayerStats {
        PlayerStats { player: 7, hits, aim_errs: vec![0.1; shots as usize], ..Default::default() }
    }

    #[test]
    fn too_few_shots_is_no_verdict_even_at_100_percent() {
        assert_eq!(AccuracyDetector.check(&player(MIN_SHOTS - 1, MIN_SHOTS - 1)), None);
    }

    #[test]
    fn at_min_shots_a_perfect_record_is_flagged() {
        let f = AccuracyDetector.check(&player(MIN_SHOTS, MIN_SHOTS)).expect("flag");
        assert_eq!((f.player, f.reason, f.value, f.samples), (7, FlagReason::Accuracy, 1.0, MIN_SHOTS));
    }

    #[test]
    fn threshold_itself_is_not_flagged_just_above_is() {
        // 100 shots so the rate lands exactly on the line.
        let at = (THRESHOLD * 100.0).round() as u32;
        assert_eq!(AccuracyDetector.check(&player(100, at)), None);
        assert!(AccuracyDetector.check(&player(100, at + 1)).is_some());
    }

    #[test]
    fn a_human_hit_rate_is_left_alone() {
        assert_eq!(AccuracyDetector.check(&player(200, 70)), None);
    }
}
