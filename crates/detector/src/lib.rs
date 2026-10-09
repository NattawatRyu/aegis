//! Aegis anomaly detector — pillar C.
//!
//! The guards stop inputs that break the rules. This crate catches players who
//! play *inside* the rules but not like a human: an aimbot sends only legal
//! inputs, so no guard can see it — its telemetry can.
//!
//! Same layout as the server's `guards/`: each detector lives in its own file
//! under [`detectors`] and owns exactly one signal. To add one, create
//! `detectors/<name>.rs`, implement [`Detector`], add a [`FlagReason`] variant
//! (and to `STANDARD`), and add one line to [`Suite::standard`].
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

pub mod monitor;
pub use monitor::{Alert, Monitor};

/// Everything the detectors know about one player, folded from telemetry.
/// Counts only, so a player who plays for hours costs the same memory as one
/// who played a minute.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlayerStats {
    pub player: u8,
    /// Inputs that reached the sim.
    pub accepted: u32,
    /// Accepted inputs a guard flagged as suspicious (e.g. a clamped move).
    pub anomalies: u32,
    pub shots: u32,
    pub hits: u32,
    /// Shots whose aim landed within `aim_exact::EXACT_RAD` of the bearing to
    /// the nearest enemy.
    pub exact: u32,
    /// Shots the server timed (`Shot::react` is `Some`): the first shot of a
    /// run of firing at an enemy just come into sight.
    pub timed: u32,
    /// Timed shots no slower than `reaction::FAST_TICKS`.
    pub fast: u32,
    /// `Glimpse` records: shots whose aim could be held against an enemy
    /// only a newer snapshot than the claimed one had shown.
    pub glimpsed: u32,
    /// Glimpses that fit such an enemy and nothing shown
    /// (`foresight::is_foreseen`).
    pub foreseen: u32,
}

impl PlayerStats {
    pub fn new(player: u8) -> Self {
        Self { player, ..Default::default() }
    }

    /// Count one outcome. `Rejected` and `Left` change nothing here: what
    /// `Left` means is up to the caller (see [`stats`] and [`Monitor`]).
    pub fn record(&mut self, o: &Outcome) {
        match *o {
            Outcome::Accepted { anomaly } => {
                self.accepted += 1;
                self.anomalies += anomaly as u32;
            }
            Outcome::Shot { hit, aim_err, react } => {
                self.shots += 1;
                self.hits += hit as u32;
                self.exact += detectors::aim_exact::is_exact(aim_err) as u32;
                self.timed += react.is_some() as u32;
                self.fast += react.is_some_and(detectors::reaction::is_fast) as u32;
            }
            Outcome::Glimpse { claimed, ahead } => {
                self.glimpsed += 1;
                self.foreseen += detectors::foresight::is_foreseen(claimed, ahead) as u32;
            }
            Outcome::Rejected { .. } | Outcome::Left => {}
        }
    }
}

/// Offline v0: fold a whole telemetry stream into per-player stats, ordered by
/// player id. Judges a whole run per id and ignores `Left`, so two people who
/// held the same id are merged — fine for the lab's scenarios, where no id is
/// reused, and wrong on a live server. [`Monitor`] is the online form; this
/// stays as its oracle.
pub fn stats(records: &[Record]) -> BTreeMap<u8, PlayerStats> {
    let mut m: BTreeMap<u8, PlayerStats> = BTreeMap::new();
    for r in records {
        m.entry(r.player).or_insert_with(|| PlayerStats::new(r.player)).record(&r.outcome);
    }
    m
}

/// Why a player was flagged. One variant per detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlagReason {
    Accuracy,
    AimExact,
    AnomalyRate,
    Reaction,
    Foresight,
}

impl FlagReason {
    /// Every reason [`Suite::standard`] can raise, for coverage checks ("does
    /// some bot trip each detector?"). Not `Accuracy`: see [`Suite::standard`].
    pub const STANDARD: [FlagReason; 4] =
        [FlagReason::AimExact, FlagReason::AnomalyRate, FlagReason::Reaction, FlagReason::Foresight];

    pub fn label(self) -> &'static str {
        match self {
            FlagReason::Accuracy => "accuracy",
            FlagReason::AimExact => "aim_exact",
            FlagReason::AnomalyRate => "anomaly_rate",
            FlagReason::Reaction => "reaction",
            FlagReason::Foresight => "foresight",
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
    /// Every detector that raises flags, in report order. This list IS the
    /// documentation of what the detector looks for.
    ///
    /// Not accuracy (2026-10-08). Once honest players react like people, an
    /// honest rusher closes in during its reaction and fires point-blank: at
    /// 900-tick crowds its online peak reached 0.985, above the humanized
    /// aimbot's 0.98. No line separates them, so accuracy stays as evidence
    /// (`detectors::accuracy`, the harness sweep's `accuracy*`) until it is
    /// normalised by range. The humanized aimbot it caught is caught by
    /// reaction instead.
    pub fn standard() -> Self {
        Self::new(vec![
            Box::new(detectors::aim_exact::AimExactDetector),
            Box::new(detectors::anomaly_rate::AnomalyRateDetector),
            Box::new(detectors::reaction::ReactionDetector),
            Box::new(detectors::foresight::ForesightDetector),
        ])
    }

    /// A suite of exactly these detectors — for tests and experiments with a
    /// detector the standard suite leaves out.
    pub fn new(detectors: Vec<Box<dyn Detector>>) -> Self {
        Self { detectors }
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
        t.shot(2, 1, true, 0.0, None);
        t.shot(3, 1, false, 0.2, None);
        t.accept(1, 2, false);
        let m = stats(t.records());
        let p1 = &m[&1];
        assert_eq!((p1.accepted, p1.anomalies, p1.shots, p1.hits, p1.exact), (2, 1, 2, 1, 1));
        assert_eq!(m[&2].shots, 0);
    }

    #[test]
    fn every_reason_has_a_detector_in_the_suite() {
        let s = Suite::standard();
        for r in FlagReason::STANDARD {
            assert!(s.detectors.iter().any(|d| d.reason() == r), "{} has no detector", r.label());
        }
        assert_eq!(s.detectors.len(), FlagReason::STANDARD.len());
    }
}
