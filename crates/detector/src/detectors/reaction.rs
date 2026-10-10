//! D4 reaction detector
//! CATCHES: anything that fires the moment an enemy is shown — triggerbots,
//!          and aimbots however well they hide their aim
//! SIGNAL:  share of timed engagements (`Shot::react` is `Some`) whose
//!          reaction is at most FAST_TICKS
//! EDGE:    timed < MIN_TIMED -> no verdict | react == FAST_TICKS is fast |
//!          the share's Wilson lower bound (Z) == THRESHOLD -> no flag |
//!          just above -> flag (5 of 9 is not; 8 of 8 is)
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

/// Flag when the share of fast reactions is, with confidence [`Z`],
/// strictly above this.
///
/// Not measured on honest play: the lab's honest players react in 6-12
/// ticks by construction, so the sweep's 0 says nothing about the margin.
/// Instant bots (aimbot, humanized, burst, triggerbot) sit at 1.0. Chosen
/// by the user 2026-10-08 with FAST_TICKS from human physiology; it must be
/// re-measured on real telemetry — anticipation rates of real players —
/// before it is trusted on a live game.
pub const THRESHOLD: f32 = 0.5;

/// How sure a flag must be that the share is above THRESHOLD: the share's
/// Wilson lower bound at this many standard errors must clear it, not the
/// raw share.
///
/// Found in the Godot demo (2026-10-10, `demos/godot`, roster with `pro`):
/// an honest player who anticipates 30% of engagements — heard the enemy
/// coming — was flagged in 5 of 9 matches on the raw share, every time
/// early, on 8–21 timed shots (5 of 9 is 0.56). The monitor checks after
/// every shot, lifetime and window, so a 6% chance per look is near
/// certain over a match. MIN_TIMED alone cannot fix it: it bounds the
/// sample, not the noise. At 2.5 an all-fast run still flags at 8 of 8
/// (bound 0.56), so instant bots are caught when they were before; 5 of 9
/// bounds at 0.21; a 78%-fast player clears 0.5 by about 40 timed shots.
pub const Z: f32 = 2.5;

/// Wilson score lower bound of `k` successes in `n` trials at `z`
/// standard errors (Wilson 1927, *Probable inference, the law of
/// succession, and statistical inference*): unlike the normal
/// approximation, sound at small `n` and at shares of 0 and 1.
pub fn wilson_lower(k: u32, n: u32, z: f32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let (n, p, z2) = (n as f64, k as f64 / n as f64, (z * z) as f64);
    let centre = p + z2 / (2.0 * n);
    let margin = z as f64 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ((centre - margin) / (1.0 + z2 / n)) as f32
}

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

    /// `fast` of `timed` engagements: flagged?
    pub fn flags(&self, fast: u32, timed: u32) -> bool {
        timed >= self.min_timed && wilson_lower(fast, timed, Z) > self.threshold
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
        if !self.cfg.flags(s.fast, s.timed) {
            return None;
        }
        let share = s.fast as f32 / s.timed as f32;
        Some(Flag {
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
        let shot = |react| Outcome::Shot { hit: true, aim_err: 0.1, react, size: 0.5 };
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

    /// `k` fast of `n` timed, the rest at a human 8 ticks.
    fn run(k: usize, n: usize) -> PlayerStats {
        let mut r = vec![0; k];
        r.extend(vec![8; n - k]);
        player(&r, 0)
    }

    #[test]
    fn wilson_bounds_at_the_edges() {
        assert_eq!(wilson_lower(0, 0, Z), 0.0);
        assert!((wilson_lower(8, 8, Z) - 0.561).abs() < 1e-3, "{}", wilson_lower(8, 8, Z));
        assert!((wilson_lower(5, 9, Z) - 0.214).abs() < 1e-3, "{}", wilson_lower(5, 9, Z));
        assert!(wilson_lower(0, 50, Z) == 0.0 && wilson_lower(50, 50, Z) < 1.0);
    }

    /// The line is the bound, not the raw share: out of 100, the first
    /// count whose bound clears THRESHOLD flags, one fewer does not — and
    /// a raw share just above THRESHOLD is far from enough.
    #[test]
    fn the_bound_at_the_line_is_not_flagged_one_more_is() {
        let k = (0..=100u32).find(|&k| wilson_lower(k, 100, Z) > THRESHOLD).unwrap() as usize;
        assert!(k > 60, "{k}");
        assert_eq!(ReactionDetector::default().check(&run(k - 1, 100)), None);
        assert!(ReactionDetector::default().check(&run(k, 100)).is_some());
        assert_eq!(ReactionDetector::default().check(&run(51, 100)), None);
    }

    /// The Godot pro's early runs (2026-10-10): flagged on the raw share.
    #[test]
    fn five_of_nine_and_the_like_are_not_enough() {
        for (k, n) in [(5, 9), (6, 10), (7, 13), (11, 21)] {
            assert_eq!(ReactionDetector::default().check(&run(k, n)), None, "{k} of {n}");
        }
    }

    fn xorshift(s: &mut u32) -> u32 {
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        *s
    }

    /// Matches of `n` timed engagements, each fast with probability `p`,
    /// judged as the monitor judges them — after every engagement, on the
    /// lifetime counts and on the last 100. The share of matches flagged
    /// at least once, by `flags`.
    fn flagged_share(p: f64, n: usize, matches: u32, flags: impl Fn(u32, u32) -> bool) -> f64 {
        let mut s = 0x2545_f491u32;
        let mut hit = 0;
        for _ in 0..matches {
            let fast: Vec<bool> = (0..n).map(|_| (xorshift(&mut s) as f64 / u32::MAX as f64) < p).collect();
            let flagged = (1..=n).any(|i| {
                let life = fast[..i].iter().filter(|&&f| f).count() as u32;
                let lo = i.saturating_sub(100);
                let win = fast[lo..i].iter().filter(|&&f| f).count() as u32;
                flags(life, i as u32) || flags(win, (i - lo) as u32)
            });
            hit += flagged as u32;
        }
        hit as f64 / matches as f64
    }

    /// An honest player who anticipates 35% of engagements, over 1000
    /// matches of 200 timed shots: the raw share flagged over a quarter, the
    /// bound almost none. A player fast on 78% (the Godot aimbot) is still
    /// caught in every match.
    #[test]
    fn an_anticipating_honest_player_is_left_alone_and_an_instant_one_is_not() {
        let cfg = Config::DEFAULT;
        let raw = |k: u32, n: u32| n >= cfg.min_timed && k as f32 / n as f32 > cfg.threshold;
        let honest_raw = flagged_share(0.35, 200, 1000, raw);
        let honest = flagged_share(0.35, 200, 1000, |k, n| cfg.flags(k, n));
        let cheat = flagged_share(0.78, 200, 1000, |k, n| cfg.flags(k, n));
        assert!(honest_raw > 0.2, "the raw share, the bug: {honest_raw}");
        assert!(honest < 0.01, "{honest}");
        assert_eq!(cheat, 1.0);
    }

    #[test]
    fn a_human_hand_is_left_alone() {
        let r: Vec<u32> = (0..60).map(|k| 6 + k % 7).collect();
        assert_eq!(ReactionDetector::default().check(&player(&r, 200)), None);
    }
}
