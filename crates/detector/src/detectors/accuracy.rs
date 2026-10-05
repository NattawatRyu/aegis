//! D1 accuracy detector
//! CATCHES: aimbots, including ones that add jitter to hide a perfect snap
//! SIGNAL:  hits / shots over the player's whole stream
//! EDGE:    shots < MIN_SHOTS -> no verdict | hit rate == THRESHOLD -> no flag |
//!          just above -> flag
//!
//! The blunt instrument: no human hits almost everything. It cannot tell a
//! great player from a subtle aimbot near the line — that is aim_exact's job,
//! and why the threshold sits well above the best honest run (see THRESHOLD).
//!
//! Weakest of the three signals, because hit rate is not only aim: range and
//! a still target matter as much. An honest rusher in a full arena hits more
//! than 0.8 of 30 shots; only a larger sample separates it from an aimbot.

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
/// (running lifetime and window, from MIN_SHOTS): honest max 0.700; the esp
/// bot (an honest rusher, culled) 0.778 in the standard scenario. Humanized
/// aimbot 0.968, snap aimbot 0.992. The line splits the 0.778-0.968 gap.
/// History: 0.8 at MIN 30, set 2026-10-02 from 4-player walker lobbies (max
/// 0.231), was wrong for a full arena — an honest whole-run rate reached
/// 0.865 there. Re-measure on real telemetry before trusting it on a live
/// game.
pub const THRESHOLD: f32 = 0.85;

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
