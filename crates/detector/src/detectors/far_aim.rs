//! D6 far-aim detector
//! CATCHES: aimbots that hide their aim and their trigger — a few ticks of
//!          delay and a little noise — and triggerbots: anything that does
//!          not miss small targets
//! SIGNAL:  share of far shots (the target's angular radius `size` under
//!          FAR_RAD) whose aim passed through the target (`aim_err <= size`)
//! EDGE:    far < MIN_FAR -> no verdict | size == FAR_RAD is not far |
//!          aim_err == size is inside | the share's Wilson lower bound (Z)
//!          == THRESHOLD -> no flag | just above -> flag
//!
//! Accuracy normalised by range. Raw hit rate left the suite (2026-10-08)
//! because an honest rusher fires point-blank, where every aim is on
//! target. Far away the target is a sliver of the view and a human hand
//! misses it often; a cheat that aims for the player, or fires only once
//! the aim is on, does not.
//!
//! Inside, not hit: both are judged in the picture the shot was chosen on
//! (`server::history`), so lag and the game's hit resolution are out of it.
//!
//! Found in the Godot demo (2026-10-10, `demos/godot`): a humanized aimbot
//! (200–400 ms, 0.012 rad of noise) and a triggerbot (human aim, machine
//! trigger) were never exact and never fast, so aim_exact and reaction
//! missed both in 9 of 9 matches. On targets under 0.04 rad they put the
//! aim inside on 92–100% of shots; honest players 27–47%, the pro 56–70%.
//!
//! **Not clean against the best honest player.** The demo's pro (0.03 rad
//! off) puts 54–78% of far shots inside over a match, the cheats 83–100%:
//! close, and a pro on a hot streak crosses any line that still catches
//! the triggerbot (see THRESHOLD). A flag here is a reason to watch the
//! replay, as every flag is, and more so than most.
//!
//! Known gaps: a cheat that misses on purpose at range escapes this
//! detector, at the price of its far kills; and a match with few far fights
//! (a small map, a melee game) gives it few samples.

use crate::detectors::reaction::wilson_lower;
use crate::{Detector, Flag, FlagReason, PlayerStats};

/// A target whose angular radius is under this is far (0.04 rad ≈ 2.3°:
/// a hitbox of radius 1 beyond 25 units, the Godot demo's 12 px beyond
/// 300 px). At 0.03 (2026-10-10, 12 Godot matches) the triggerbot had too
/// few far shots and was caught 0 times in 12.
pub const FAR_RAD: f32 = 0.04;

/// Fewer far shots than this and the share is noise. 12: the shortest run,
/// all inside, whose bound clears THRESHOLD (12/(12+Z²) = 0.66).
pub const MIN_FAR: u32 = 12;

/// Flag when the share of far shots aimed inside is, with confidence
/// [`Z`], strictly above this.
///
/// Measured in the Godot demo (2026-10-10, roster of 2 honest, 2 pro,
/// aimbot, humanized, triggerbot; 9000 ticks; honest and pro runs flagged,
/// cheats caught):
///
/// | line | matches | honest+pro flagged | humanized | trigger |
/// |------|---------|--------------------|-----------|---------|
/// | 0.60 | 12      | 1 of 48            | 12/12     | 11/12   |
/// | 0.65 | 24      | 2 of 96            | 24/24     | 19/24   |
/// | 0.70 | 24      | 1 of 96            | 24/24     | 10/24   |
/// | 0.75 | 24      | 1 of 96            | 21/24     | 5/24    |
///
/// Every false flag was a pro, never an honest player. Raising the line
/// barely moves them — they are streaks (12 of 12, 25 of 27) — and costs
/// the triggerbot fast, so 0.65. In the lab (`aegis-harness sweep`, 1008
/// honest players) the highest online bound was 0.333 at round trip 0 and
/// 0.397 at 6 ticks: nobody flagged. A game with better players than the
/// demo's pro must raise it from its own telemetry.
pub const THRESHOLD: f32 = 0.65;

/// Standard errors of confidence a flag needs: the Wilson lower bound of the
/// share at this Z must clear THRESHOLD. Same as reaction's, for the same
/// reason — the monitor looks after every shot.
pub const Z: f32 = 2.5;

/// This detector's lines, per game; the consts above are the defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub far_rad: f32,
    pub min_far: u32,
    pub threshold: f32,
}

impl Config {
    pub const DEFAULT: Self = Self { far_rad: FAR_RAD, min_far: MIN_FAR, threshold: THRESHOLD };

    /// Is a target of angular radius `size` far?
    pub fn is_far(&self, size: f32) -> bool {
        size < self.far_rad
    }

    /// Would `inside` of `far` far shots be flagged?
    pub fn flags(&self, inside: u32, far: u32) -> bool {
        far >= self.min_far && wilson_lower(inside, far, Z) > self.threshold
    }
}

/// Did an aim `aim_err` off the bearing pass through a target of angular
/// radius `size`?
pub fn is_inside(aim_err: f32, size: f32) -> bool {
    aim_err <= size
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Default)]
pub struct FarAimDetector {
    pub cfg: Config,
}

impl Detector for FarAimDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::FarAim
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        self.cfg.flags(s.far_inside, s.far).then(|| Flag {
            player: s.player,
            reason: FlagReason::FarAim,
            value: s.far_inside as f32 / s.far as f32,
            threshold: self.cfg.threshold,
            samples: s.far,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_telemetry::Outcome;

    /// `inside` far shots through the target, then `outside` far shots
    /// missing it, counted the way the server's telemetry would be.
    fn player(inside: u32, outside: u32) -> PlayerStats {
        let mut s = PlayerStats::new(4);
        let size = FAR_RAD / 2.0;
        for k in 0..inside + outside {
            let aim_err = if k < inside { size } else { size * 1.5 };
            s.record(&Outcome::Shot { hit: k < inside, aim_err, react: None, size });
        }
        s
    }

    fn flagged(s: &PlayerStats) -> bool {
        FarAimDetector::default().check(s).is_some()
    }

    #[test]
    fn too_few_far_shots_is_no_verdict_even_if_all_inside() {
        assert!(!flagged(&player(MIN_FAR - 1, 0)));
        let f = FarAimDetector::default().check(&player(MIN_FAR, 0)).expect("flag");
        assert_eq!((f.reason, f.value, f.samples), (FlagReason::FarAim, 1.0, MIN_FAR));
    }

    #[test]
    fn far_rad_itself_is_not_far_and_aim_on_the_edge_is_inside() {
        let mut s = PlayerStats::new(4);
        for _ in 0..50 {
            s.record(&Outcome::Shot { hit: true, aim_err: 0.0, react: None, size: FAR_RAD });
        }
        assert_eq!((s.far, flagged(&s)), (0, false), "not far: no evidence");
        assert!(is_inside(0.01, 0.01) && !is_inside(0.010001, 0.01));
    }

    /// The bound, not the share: 7 of 10 is 0.7 raw, but bounds at ~0.35.
    #[test]
    fn the_bound_at_the_line_is_not_flagged_one_more_is() {
        let c = Config::DEFAULT;
        let n = 40;
        let k = (0..=n).find(|&k| wilson_lower(k, n, Z) > THRESHOLD).expect("some k flags");
        assert!(!c.flags(k - 1, n) && c.flags(k, n), "k {k} of {n}");
        assert!(!c.flags(7, 10));
    }

    #[test]
    fn near_shots_are_left_out_whatever_they_hit() {
        let mut s = player(0, 20);
        for _ in 0..200 {
            s.record(&Outcome::Shot { hit: true, aim_err: 0.0, react: None, size: 0.5 });
        }
        assert_eq!((s.far, s.far_inside), (20, 0));
        assert!(!flagged(&s), "point-blank hits are not evidence");
    }

    /// The Godot finding, as numbers: honest and pro shares over a match's
    /// far shots stay under; the smart cheats' do not.
    #[test]
    fn godot_shares_are_split_by_the_line() {
        let c = Config::DEFAULT;
        for (inside, far) in [(17, 37), (16, 35), (42, 60), (46, 69), (44, 63)] {
            assert!(!c.flags(inside, far), "honest or pro {inside}/{far}");
        }
        for (inside, far) in [(41, 44), (49, 53), (14, 14), (21, 21)] {
            assert!(c.flags(inside, far), "cheat {inside}/{far}");
        }
    }
}
