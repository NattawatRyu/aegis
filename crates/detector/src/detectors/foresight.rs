//! D5 foresight detector
//! CATCHES: a client that claims an older snapshot than the one it acts on
//!          (`StaleLiar`) — the dodge that hides an instant reaction from
//!          [`super::reaction`]
//! SIGNAL:  count of `Glimpse` records whose aim fits an enemy only a newer
//!          snapshot had shown (within FIT_RAD) while nothing the claimed
//!          snapshot showed is within CLEAR_RAD of it
//! EDGE:    foreseen < MIN_FORESEEN -> no flag | == MIN_FORESEEN -> flag |
//!          ahead == FIT_RAD fits | claimed == CLEAR_RAD is not clear
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
//! So MIN_FORESEEN must be re-measured on real telemetry before it is
//! trusted; and a lifetime count grows with play, so a long session needs a
//! rate, not a count.
//!
//! Known gaps:
//!   - a liar that keeps to enemies already in its claimed picture and only
//!     aims at their newer positions reads as extrapolation: not caught, and
//!     it buys a little precision, not a reaction;
//!   - one that holds fire until the enemy reaches its claimed picture has
//!     no foresight — and then really is that slow, which is
//!     [`super::reaction`]'s business;
//!   - at round trip 3, 2 of 8 one-tick liars and 1 of 8 two-tick liars
//!     foresaw only twice in 900 ticks (30 s), under MIN_FORESEEN.

use crate::{Detector, Flag, FlagReason, PlayerStats};

/// An aim within this many radians of an enemy fits it: the lab's honest
/// hand (`client_sdk::honest::AIM_ERROR_RAD`, 0.15), the widest a human aim
/// at that enemy is off.
pub const FIT_RAD: f32 = 0.15;

/// Every enemy the claimed snapshot showed must be further off the aim than
/// this — FIT_RAD plus 0.05 — for the aim to be unexplained by it.
pub const CLEAR_RAD: f32 = 0.2;

/// Flag at this many foreseen shots. 2 caught every liar run measured; 3 is
/// one shot of margin for prefire luck, at the cost of 3 of 224 liar runs.
pub const MIN_FORESEEN: u32 = 3;

/// A glimpse whose aim fits an unshown enemy and nothing shown.
pub fn is_foreseen(claimed: Option<f32>, ahead: f32) -> bool {
    ahead <= FIT_RAD && claimed.is_none_or(|c| c > CLEAR_RAD)
}

pub struct ForesightDetector;

impl Detector for ForesightDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::Foresight
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        (s.foreseen >= MIN_FORESEEN).then_some(Flag {
            player: s.player,
            reason: FlagReason::Foresight,
            value: s.foreseen as f32,
            threshold: MIN_FORESEEN as f32,
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

    #[test]
    fn one_short_of_min_is_no_flag_min_is() {
        let mut g = vec![(None, 0.0); MIN_FORESEEN as usize - 1];
        g.extend(vec![(Some(0.05), 0.0); 200]);
        assert_eq!(ForesightDetector.check(&player(&g)), None, "explained glimpses counted");
        g.push((Some(0.5), 0.1));
        let f = ForesightDetector.check(&player(&g)).expect("flag");
        assert_eq!(
            (f.player, f.reason, f.value, f.samples),
            (9, FlagReason::Foresight, MIN_FORESEEN as f32, MIN_FORESEEN + 200)
        );
    }

    #[test]
    fn an_honest_hand_is_left_alone() {
        // Off its shown target by up to the hand, a newer enemy nearer still.
        let g: Vec<_> = (0..300).map(|k| (Some(FIT_RAD * (k % 10) as f32 / 10.0), 0.0)).collect();
        assert_eq!(ForesightDetector.check(&player(&g)), None);
    }
}
