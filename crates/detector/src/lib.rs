//! Aegis anomaly detector — pillar C.
//!
//! The guards stop inputs that break the rules. This crate catches players who
//! play *inside* the rules but not like a human: an aimbot sends only legal
//! inputs, so no guard can see it — its telemetry can.
//!
//! Same layout as the server's `guards/`: each detector lives in its own file
//! under [`detectors`] and owns exactly one signal. To add one, create
//! `detectors/<name>.rs`, implement [`Detector`], add a [`FlagReason`] variant
//! (and to `ALL`), and add one line to [`Suite::standard`].
//!
//! Reads telemetry [`Record`]s only — never the server crate — so the server
//! never has to know what the detector looks for. Thresholds are measured, not
//! guessed: each detector's `THRESHOLD` doc cites the honest sweep
//! (`aegis-harness sweep`) it was set from.
//!
//! A [`Flag`] is evidence for a human reviewer, not a ban. Every detector
//! refuses to judge a player with fewer samples than its minimum: a player who
//! fired 3 shots and hit all 3 is lucky, not an aimbot.

use std::collections::BTreeMap;

use aegis_telemetry::{Outcome, Record};

pub mod detectors;

/// Everything the detectors know about one player, folded from telemetry.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PlayerStats {
    pub player: u8,
    /// Inputs that reached the sim.
    pub accepted: u32,
    /// Accepted inputs a guard flagged as suspicious (e.g. a clamped move).
    pub anomalies: u32,
    pub hits: u32,
    /// One entry per shot, radians off the bearing to the nearest enemy.
    pub aim_errs: Vec<f32>,
}

impl PlayerStats {
    pub fn shots(&self) -> u32 {
        self.aim_errs.len() as u32
    }
}

/// Fold a telemetry stream into per-player stats, ordered by player id.
pub fn stats(records: &[Record]) -> BTreeMap<u8, PlayerStats> {
    let mut m: BTreeMap<u8, PlayerStats> = BTreeMap::new();
    for r in records {
        let s = m.entry(r.player).or_insert_with(|| PlayerStats { player: r.player, ..Default::default() });
        match r.outcome {
            Outcome::Accepted { anomaly } => {
                s.accepted += 1;
                s.anomalies += anomaly as u32;
            }
            Outcome::Rejected { .. } => {}
            Outcome::Shot { hit, aim_err } => {
                s.hits += hit as u32;
                s.aim_errs.push(aim_err);
            }
        }
    }
    m
}

/// Why a player was flagged. One variant per detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlagReason {
    Accuracy,
    AimExact,
    AnomalyRate,
}

impl FlagReason {
    /// Every reason, for coverage checks ("does some bot trip each detector?").
    pub const ALL: [FlagReason; 3] = [FlagReason::Accuracy, FlagReason::AimExact, FlagReason::AnomalyRate];

    pub fn label(self) -> &'static str {
        match self {
            FlagReason::Accuracy => "accuracy",
            FlagReason::AimExact => "aim_exact",
            FlagReason::AnomalyRate => "anomaly_rate",
        }
    }
}

/// One detector's verdict on one player: the measured value, the line it
/// crossed, and how many samples it was measured over — enough for a reviewer
/// to check the call without rerunning anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    pub player: u8,
    pub reason: FlagReason,
    pub value: f32,
    pub threshold: f32,
    pub samples: u32,
}

pub trait Detector {
    fn reason(&self) -> FlagReason;
    /// `Some` iff the player has enough samples AND crosses the threshold.
    fn check(&self, s: &PlayerStats) -> Option<Flag>;
}

pub struct Suite {
    detectors: Vec<Box<dyn Detector>>,
}

impl Suite {
    /// Every detector, in report order. This list IS the documentation of what
    /// the detector looks for.
    pub fn standard() -> Self {
        Self {
            detectors: vec![
                Box::new(detectors::accuracy::AccuracyDetector),
                Box::new(detectors::aim_exact::AimExactDetector),
                Box::new(detectors::anomaly_rate::AnomalyRateDetector),
            ],
        }
    }

    pub fn check(&self, s: &PlayerStats) -> Vec<Flag> {
        self.detectors.iter().filter_map(|d| d.check(s)).collect()
    }

    /// Every flag for every player in the stream.
    pub fn run(&self, records: &[Record]) -> Vec<Flag> {
        stats(records).values().flat_map(|s| self.check(s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_telemetry::Telemetry;

    #[test]
    fn stats_fold_every_outcome_per_player() {
        let mut t = Telemetry::new();
        t.accept(1, 1, false);
        t.accept(2, 1, true);
        t.reject(2, 1, "replay");
        t.shot(2, 1, true, 0.0);
        t.shot(3, 1, false, 0.2);
        t.accept(1, 2, false);
        let m = stats(t.records());
        let p1 = &m[&1];
        assert_eq!((p1.accepted, p1.anomalies, p1.hits, p1.shots()), (2, 1, 1, 2));
        assert_eq!(p1.aim_errs, vec![0.0, 0.2]);
        assert_eq!(m[&2].shots(), 0);
    }

    #[test]
    fn every_reason_has_a_detector_in_the_suite() {
        let s = Suite::standard();
        for r in FlagReason::ALL {
            assert!(s.detectors.iter().any(|d| d.reason() == r), "{} has no detector", r.label());
        }
        assert_eq!(s.detectors.len(), FlagReason::ALL.len());
    }
}
