//! D1 accuracy detector
//! CATCHES: aimbots, including ones that add jitter to hide a perfect snap
//! SIGNAL:  hits / shots over the player's whole stream
//! EDGE:    shots < MIN_SHOTS -> no verdict | hit rate == THRESHOLD -> no flag |
//!          just above -> flag
//!
//! The blunt instrument: no human hits almost everything. It cannot tell a
//! great player from a subtle aimbot near the line — that is aim_exact's job.
//! The threshold sits just above the best honest run, not well above it (see
//! THRESHOLD).
//!
//! Weakest of the three signals, because hit rate is not only aim: range and
//! a still target matter as much. **Not in `Suite::standard` since 2026-10-08:**
//! with a human reaction an honest rusher closes in and fires point-blank,
//! and at 900-tick crowds it peaked at 0.985 — above the humanized aimbot.
//! Kept as evidence, and to come back once hit rate is normalised by range.

use crate::{Detector, Flag, FlagReason, PlayerStats};

/// Fewer shots than this and hit rate is noise, not evidence.
///
/// 60, not 30: at 30 shots an honest rusher's online peak reached 0.914
/// (`aegis-harness sweep`, 63 crowds of 16 = 1008 honest players, walkers,
/// campers and rushers, 2026-10-05); at 60 its max is 0.700. The cost: 46% of
/// that crowd fired fewer than 60 aim-evidence shots and gets no accuracy
/// verdict — aim_exact (30) still judges them.
pub const MIN_SHOTS: u32 = 60;

/// Flag strictly above this hit rate.
///
/// Measured on the same crowd, over every view the online monitor judges
/// (running lifetime and window, from MIN_SHOTS), with honest players taking
/// a 6-12 tick reaction (C6.3, 2026-10-08, 600-tick crowds): honest max
/// 0.934 — rushers keep closing during the reaction and fire their first
/// shots point-blank. Humanized and snap aimbot both 0.98 in the standard
/// scenario. A thin margin, and it broke the same day: at 900-tick crowds
/// 17 honest rushers peaked above it (max 0.985), so the user dropped
/// accuracy from the standard suite rather than raise the line past the
/// cheaters it was meant to catch.
/// History: 0.8 at MIN 30 (2026-10-02, 4-player walker lobbies) was wrong
/// for a full arena; 0.85 (2026-10-05, honest max 0.700) was wrong once
/// honest players reacted like people; 0.95 was wrong once they played long
/// enough to reach their tail. Re-measure on real telemetry before trusting
/// any line on a live game.
pub const THRESHOLD: f32 = 0.95;

pub struct AccuracyDetector;

impl Detector for AccuracyDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::Accuracy
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        let shots = s.shots;
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
        PlayerStats { player: 7, shots, hits, ..Default::default() }
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
