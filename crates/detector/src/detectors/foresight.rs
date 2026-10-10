//! D5 foresight detector
//! CATCHES: a client that claims an older snapshot than the one it acts on
//!          (`StaleLiar`) — the dodge that hides an instant reaction from
//!          [`super::reaction`]
//! SIGNAL:  share of `Glimpse` records whose aim fits an enemy only a newer
//!          snapshot had shown (within FIT_RAD) while nothing the claimed
//!          snapshot showed is within CLEAR_RAD of it — over the life and
//!          over the monitor's last 100 glimpses
//! EDGE:    foreseen < MIN_FORESEEN -> no verdict | share == THRESHOLD ->
//!          no flag | just above -> flag | ahead == FIT_RAD fits |
//!          claimed == CLEAR_RAD is not clear
//!
//! Every input proves which snapshot it was chosen on, and the server judges
//! the shot in that picture (`server::history`). A proof for an *older*
//! snapshot the client did receive is real, so claiming to be laggier than
//! it is cannot be refused by time: a laggy player looks the same. It is
//! refused by content. A client has nothing newer than the snapshot it
//! claims, and no extrapolation finds an enemy it has never been shown; an
//! aim at such an enemy, with nobody in its claimed picture to explain it,
//! was chosen on a picture it says it did not have.
//!
//! Measured with `aegis-harness foresight 8 6` (2026-10-09), round trips
//! 0..=6 ticks, 8 crowds each of `lag_mix` and `stale_mix`: honest walkers,
//! campers and rushers — and the burst and triggerbot, which aim with the
//! same hand — foresaw 0 times in 1344 player-runs; every `StaleLiar` (1, 2,
//! 3 and 6 ticks behind) foresaw at least 2 times in every run, at least 3
//! in 221 of 224. The first rule tried, "fits the newer enemy better than
//! anyone shown, by a margin", read 1–7% of an honest player's open shots
//! as foresight: an honest aim is off its target by up to the hand's
//! 0.15 rad, and a new enemy near that bearing beat it.
//!
//! The honest 0 is by construction, not a measured margin: the lab's honest
//! players never fire with nobody in sight, so `claimed` is always within
//! their hand (0.15 rad) and never clear. Real players prefire corners on
//! sound and on game sense, and someone does sometimes walk into that aim.
//! So the line is a share, not a count (2026-10-09): a count grows with
//! play and a long enough honest session reaches any count; a share does
//! not. MIN_FORESEEN 3 and THRESHOLD 2% over the life, and the same over
//! the last 100 glimpses — so 3 foreseen inside any 100 flag, however long
//! the straight play before. Measured with `aegis-harness lag 8 6 stale`:
//! liars 218 of 224 runs flagged (count-only: 221), honest 0 of 1344; the
//! 6 missed are all at round trip 3 in 30 s. Every line here is the game's
//! to set (`Config`): the radii follow its hitboxes and range, and
//! `crate::Config::validate` refuses a set that cannot mean anything.
//!
//! Known gaps:
//!   - a liar that keeps to enemies already in its claimed picture and only
//!     aims at their newer positions reads as extrapolation: not caught, and
//!     it buys a little precision, not a reaction;
//!   - one that holds fire until the enemy reaches its claimed picture has
//!     no foresight — and then really is that slow, which is
//!     [`super::reaction`]'s business;
//!   - at round trip 3, 4 of 8 one-tick liars and 1 of 8 two- and
//!     three-tick liars went unflagged in 900 ticks (30 s): too few
//!     foreseen, or spread too thin for the share.

use crate::{Detector, Flag, FlagReason, PlayerStats};

/// An aim within this many radians of an enemy fits it: the lab's honest
/// hand (`client_sdk::honest::AIM_ERROR_RAD`, 0.15), the widest a human aim
/// at that enemy is off.
pub const FIT_RAD: f32 = 0.15;

/// Every enemy the claimed snapshot showed must be further off the aim than
/// this — FIT_RAD plus 0.05 — for the aim to be unexplained by it.
pub const CLEAR_RAD: f32 = 0.2;

/// No verdict under this many foreseen shots, whatever the share. 2 caught
/// every liar run measured; 3 is one shot of margin for prefire luck.
pub const MIN_FORESEEN: u32 = 3;

/// Flag strictly above this share of glimpses foreseen. A count alone grows
/// with play: an honest player whose prefire some enemy walks into once in
/// a few hundred glimpses would reach any count in a long enough session.
/// A share does not, and the monitor's window (the last 100 glimpses) still
/// sees a liar who foresees in bursts: 3 in 100 is already over it.
pub const THRESHOLD: f32 = 0.02;

/// A glimpse whose aim fits an unshown enemy and nothing shown, at the
/// default radii. See [`Config::is_foreseen`].
pub fn is_foreseen(claimed: Option<f32>, ahead: f32) -> bool {
    Config::DEFAULT.is_foreseen(claimed, ahead)
}

/// This detector's lines, per game; the consts above are the defaults.
///
/// The radii are the game's: FIT_RAD is how far off a human aim at an enemy
/// is (wide in a game of big hitboxes and close range, narrow in a sniper
/// game), and CLEAR_RAD must stay above it — see [`crate::Config::validate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub fit_rad: f32,
    pub clear_rad: f32,
    pub min_foreseen: u32,
    pub threshold: f32,
}

impl Config {
    pub const DEFAULT: Self =
        Self { fit_rad: FIT_RAD, clear_rad: CLEAR_RAD, min_foreseen: MIN_FORESEEN, threshold: THRESHOLD };

    pub fn is_foreseen(&self, claimed: Option<f32>, ahead: f32) -> bool {
        ahead <= self.fit_rad && claimed.is_none_or(|c| c > self.clear_rad)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Default)]
pub struct ForesightDetector {
    pub cfg: Config,
}

impl Detector for ForesightDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::Foresight
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        if s.foreseen < self.cfg.min_foreseen {
            return None;
        }
        let share = s.foreseen as f32 / s.glimpsed as f32;
        (share > self.cfg.threshold).then_some(Flag {
            player: s.player,
            reason: FlagReason::Foresight,
            value: share,
            threshold: self.cfg.threshold,
            samples: s.glimpsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_telemetry::Outcome;

    fn player(glimpses: &[(Option<f32>, f32)]) -> PlayerStats {
        let mut s = PlayerStats::new(9);
        for &(claimed, ahead) in glimpses {
            s.record(&Outcome::Glimpse { claimed, ahead });
        }
        s
    }

    #[test]
    fn the_edges_of_one_glimpse() {
        assert!(is_foreseen(None, FIT_RAD), "FIT_RAD itself fits");
        assert!(!is_foreseen(None, FIT_RAD + 1e-6));
        assert!(!is_foreseen(Some(CLEAR_RAD), 0.0), "CLEAR_RAD itself is not clear");
        assert!(is_foreseen(Some(CLEAR_RAD + 1e-6), 0.0));
        assert!(!is_foreseen(Some(0.0), 0.0), "a shown enemy right on the aim explains it");
    }

    /// `foreseen` glimpses foreseen, then explained ones up to `total`.
    fn mix(foreseen: usize, total: usize) -> PlayerStats {
        let mut g = vec![(None, 0.0); foreseen];
        g.extend(vec![(Some(0.05), 0.0); total - foreseen]);
        player(&g)
    }

    #[test]
    fn one_short_of_min_is_no_flag_min_is() {
        let n = MIN_FORESEEN as usize;
        assert_eq!(ForesightDetector::default().check(&mix(n - 1, n - 1)), None, "all foreseen, too few");
        let f = ForesightDetector::default().check(&mix(n, n + 2)).expect("flag");
        assert_eq!((f.player, f.reason, f.samples), (9, FlagReason::Foresight, MIN_FORESEEN + 2));
    }

    #[test]
    fn threshold_itself_is_not_flagged_just_above_is() {
        let n = MIN_FORESEEN as usize;
        let at = (n as f32 / THRESHOLD).round() as usize; // n of `at` is exactly the share
        assert_eq!(ForesightDetector::default().check(&mix(n, at)), None);
        assert!(ForesightDetector::default().check(&mix(n, at - 1)).is_some());
    }

    /// The reason for a share: one coincidence every 100 glimpses, over a
    /// long session, piles up a count far past MIN_FORESEEN and is still
    /// not flagged.
    #[test]
    fn a_rare_coincidence_over_a_long_session_is_left_alone() {
        let mut s = PlayerStats::new(9);
        for k in 0..5_000 {
            let claimed = if k % 100 == 0 { None } else { Some(0.05) };
            s.record(&Outcome::Glimpse { claimed, ahead: 0.0 });
        }
        assert_eq!(s.foreseen, 50);
        assert_eq!(ForesightDetector::default().check(&s), None);
    }

    #[test]
    fn radii_are_the_games() {
        let wide = Config { fit_rad: 0.4, clear_rad: 0.5, ..Config::DEFAULT };
        assert!(wide.is_foreseen(Some(0.6), 0.3) && !is_foreseen(Some(0.6), 0.3));
        assert!(!wide.is_foreseen(Some(0.45), 0.0), "inside the game's clear radius");
    }

    #[test]
    fn an_honest_hand_is_left_alone() {
        // Off its shown target by up to the hand, a newer enemy nearer still.
        let g: Vec<_> = (0..300).map(|k| (Some(FIT_RAD * (k % 10) as f32 / 10.0), 0.0)).collect();
        assert_eq!(ForesightDetector::default().check(&player(&g)), None);
    }
}
