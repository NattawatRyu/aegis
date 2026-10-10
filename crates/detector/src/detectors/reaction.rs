//! D4 reaction detector
//! CATCHES: anything that fires the moment an enemy is shown — triggerbots,
//!          and aimbots however well they hide their aim
//! SIGNAL:  share of timed engagements (`Shot::react` is `Some`) whose
//!          reaction is at most FAST_TICKS
//! EDGE:    timed < MIN_TIMED -> no verdict | react == FAST_TICKS is fast |
//!          share == THRESHOLD -> no flag | just above -> flag
//!
//! The server times an engagement only when the shot starts firing: not
//! prefire, not a switch from another target (`server::reaction`). So every
//! timed shot is a player at rest who saw an enemy and pulled the trigger,
//! and how long that took is bounded below by the human body, not by skill.
//!
//! Latency is out of it (C6.7, 2026-10-09). Every shot is judged in the
//! picture the client's input proves it chose on (`server::history`): which
//! enemy, how far off, and how long since that enemy appeared, all in that
//! snapshot's ticks. Measured with `aegis-harness lag` at round trips 0..=6
//! ticks (0–200 ms): instant bots read 0 on every timed engagement and
//! honest players never under 6; 1008 honest players at 3 and at 6 flagged
//! 0 times. (Before, a lagged shot was scored against the server's world
//! *now*: honest walkers read instant — 14 of 24 flagged at 6 — and once
//! time was corrected without the target, instant bots went untimed.)
//!
//! Known gaps:
//!   - a client that claims a picture a few ticks older than it has (real,
//!     proven, inside the history) turns instant shots into prefire and
//!     escapes this detector (`StaleLiar` in the harness) — it is
//!     [`super::foresight`]'s to catch, by aim at enemies the claimed
//!     picture never showed;
//!   - on a lossy link a lost snapshot breaks a run of fire, so a run can
//!     read as two (not measured: the lab does not drop);
//!   - a cheater who sprays without pause (all prefire, nothing timed);
//!   - fewer than MIN_TIMED timed engagements in a match: at round trips
//!     2–4, 0–2 of 8 triggerbots stay unjudged in 900 ticks at MIN_TIMED 8
//!     (3–5 at 10).
//!
//! Every line is the game's to set (`Config`, `crate::Config`): FAST_TICKS
//! is 100 ms at 30 Hz, and `Config::at_tick_rate` converts it.

use crate::{Detector, Flag, FlagReason, PlayerStats};

/// A reaction this fast or faster is not a reaction: 3 ticks is 100 ms at
/// 30 Hz, the false-start line in sprinting (World Athletics: a start under
/// 0.100 s after the gun is ruled a false start, because no one hears and
/// moves that fast). A human that quick guessed — pre-aimed and clicked on
/// a hunch — and guesses are a minority of engagements.
pub const FAST_TICKS: u32 = 3;

/// Fewer timed engagements than this and the share is noise.
///
/// 8, the middle of the measured range (2026-10-09, `aegis-harness lag 8 6`,
/// 900-tick crowds): at 10 only 3–7 of 8 triggerbots got a verdict at round
/// trips 2–4 — a lagged engagement starts less often from rest — at 8, 6–8
/// of 8; at 6, 7–8. The cost is noise: a player who guesses on 1 engagement
/// in 10 crosses THRESHOLD (5 of 8) with probability ~4e-4 per verdict, vs
/// ~1.5e-4 at 10 and ~1.3e-3 at 6. A game with long matches can raise it
/// (`Config::min_timed`); one with short rounds can lower it.
pub const MIN_TIMED: u32 = 8;

/// Flag strictly above this share of fast reactions.
///
/// Not measured on honest play: the lab's honest players react in 6-12
/// ticks by construction, so the sweep's 0 says nothing about the margin.
/// Instant bots (aimbot, humanized, burst, triggerbot) sit at 1.0. Chosen
/// by the user 2026-10-08 with FAST_TICKS from human physiology; it must be
/// re-measured on real telemetry — anticipation rates of real players —
/// before it is trusted on a live game.
pub const THRESHOLD: f32 = 0.5;

/// The fastest a human reacts, in milliseconds: what FAST_TICKS is at
/// 30 Hz, and what [`Config::at_tick_rate`] converts for other rates.
pub const FAST_MS: u32 = 100;

/// Is a reaction fast, at the default [`FAST_TICKS`]? See [`Config::is_fast`].
pub fn is_fast(react: u32) -> bool {
    Config::DEFAULT.is_fast(react)
}

/// This detector's lines, per game; the consts above are the defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// In the game's ticks: [`Config::at_tick_rate`] sets it from FAST_MS.
    pub fast_ticks: u32,
    pub min_timed: u32,
    pub threshold: f32,
}

impl Config {
    pub const DEFAULT: Self = Self { fast_ticks: FAST_TICKS, min_timed: MIN_TIMED, threshold: THRESHOLD };

    /// The defaults for a server ticking `hz` times a second: FAST_MS in its
    /// ticks, rounded down (a reaction is timed in whole ticks, and rounding
    /// up would call a human at the line fast), at least 1.
    pub fn at_tick_rate(hz: u32) -> Self {
        Self { fast_ticks: (FAST_MS * hz / 1000).max(1), ..Self::DEFAULT }
    }

    pub fn is_fast(&self, react: u32) -> bool {
        react <= self.fast_ticks
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Default)]
pub struct ReactionDetector {
    pub cfg: Config,
}

impl Detector for ReactionDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::Reaction
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        if s.timed < self.cfg.min_timed {
            return None;
        }
        let share = s.fast as f32 / s.timed as f32;
        (share > self.cfg.threshold).then_some(Flag {
            player: s.player,
            reason: FlagReason::Reaction,
            value: share,
            threshold: self.cfg.threshold,
            samples: s.timed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_telemetry::Outcome;

    /// One shot per reaction in `reacts`, plus `untimed` shots with none.
    fn player(reacts: &[u32], untimed: usize) -> PlayerStats {
        let mut s = PlayerStats::new(4);
        let shot = |react| Outcome::Shot { hit: true, aim_err: 0.1, react };
        reacts.iter().for_each(|&k| s.record(&shot(Some(k))));
        (0..untimed).for_each(|_| s.record(&shot(None)));
        s
    }

    #[test]
    fn too_few_timed_is_no_verdict_even_if_all_instant() {
        let r = vec![0; MIN_TIMED as usize - 1];
        assert_eq!(ReactionDetector::default().check(&player(&r, 500)), None, "untimed shots counted as samples");
    }

    #[test]
    fn all_instant_at_min_timed_is_flagged() {
        let f = ReactionDetector::default().check(&player(&vec![0; MIN_TIMED as usize], 0)).expect("flag");
        assert_eq!((f.player, f.reason, f.value, f.samples), (4, FlagReason::Reaction, 1.0, MIN_TIMED));
    }

    #[test]
    fn fast_ticks_itself_is_fast_one_more_is_not() {
        let n = MIN_TIMED as usize;
        assert!(ReactionDetector::default().check(&player(&vec![FAST_TICKS; n], 0)).is_some());
        assert_eq!(ReactionDetector::default().check(&player(&vec![FAST_TICKS + 1; n], 0)), None);
    }

    #[test]
    fn threshold_itself_is_not_flagged_just_above_is() {
        let at = (THRESHOLD * 100.0).round() as usize;
        let mut r = vec![0; at];
        r.extend(vec![8; 100 - at]);
        assert_eq!(ReactionDetector::default().check(&player(&r, 0)), None);
        r[at] = 0;
        assert!(ReactionDetector::default().check(&player(&r, 0)).is_some());
    }

    #[test]
    fn a_human_hand_is_left_alone() {
        let r: Vec<u32> = (0..60).map(|k| 6 + k % 7).collect();
        assert_eq!(ReactionDetector::default().check(&player(&r, 200)), None);
    }
}
