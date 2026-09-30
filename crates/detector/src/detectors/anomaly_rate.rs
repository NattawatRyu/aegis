//! D3 anomaly-rate detector
//! CATCHES: speedhacks and other tampered clients the guards neutralise but
//!          let through (a clamped move vector is flagged, not rejected)
//! SIGNAL:  anomalies / accepted inputs
//! EDGE:    accepted < MIN_INPUTS -> no verdict | rate == THRESHOLD -> no
//!          flag | just above -> flag
//!
//! The guard already made the cheat useless; this turns "blocked" into "seen".
//! A clamp can fire on an honest float rounding edge (the reason move_speed
//! flags instead of rejects), so one or two are expected — the threshold is a
//! share, not a count.

use crate::{Detector, Flag, FlagReason, PlayerStats};

pub const MIN_INPUTS: u32 = 30;

/// Flag strictly above this share of anomalous inputs. Honest bots produce
/// zero (sweep 250: max 0.000), so the value is a judgment about real
/// clients, not a measurement: one input in five tampered is not a rounding
/// edge.
pub const THRESHOLD: f32 = 0.2;

pub struct AnomalyRateDetector;

impl Detector for AnomalyRateDetector {
    fn reason(&self) -> FlagReason {
        FlagReason::AnomalyRate
    }

    fn check(&self, s: &PlayerStats) -> Option<Flag> {
        if s.accepted < MIN_INPUTS {
            return None;
        }
        let rate = s.anomalies as f32 / s.accepted as f32;
        (rate > THRESHOLD).then_some(Flag {
            player: s.player,
            reason: FlagReason::AnomalyRate,
            value: rate,
            threshold: THRESHOLD,
            samples: s.accepted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(accepted: u32, anomalies: u32) -> PlayerStats {
        PlayerStats { player: 2, accepted, anomalies, ..Default::default() }
    }

    #[test]
    fn too_few_inputs_is_no_verdict() {
        assert_eq!(AnomalyRateDetector.check(&player(MIN_INPUTS - 1, MIN_INPUTS - 1)), None);
    }

    #[test]
    fn every_input_anomalous_is_flagged() {
        let f = AnomalyRateDetector.check(&player(300, 300)).expect("flag");
        assert_eq!((f.reason, f.value, f.samples), (FlagReason::AnomalyRate, 1.0, 300));
    }

    #[test]
    fn threshold_itself_is_not_flagged_just_above_is() {
        let at = (THRESHOLD * 100.0).round() as u32;
        assert_eq!(AnomalyRateDetector.check(&player(100, at)), None);
        assert!(AnomalyRateDetector.check(&player(100, at + 1)).is_some());
    }

    #[test]
    fn a_rare_rounding_edge_is_tolerated() {
        assert_eq!(AnomalyRateDetector.check(&player(300, 2)), None);
    }
}
