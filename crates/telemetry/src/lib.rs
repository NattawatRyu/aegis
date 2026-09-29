//! Aegis telemetry — the evidence trail every guard verdict leaves behind.
//!
//! Two consumers, one recorder:
//!   - the **detector** (pillar C) reads the per-player feature stream to learn
//!     what a cheater looks like over time;
//!   - the **harness** reads [`Totals`] to assert "the speedhack was blocked".
//!
//! This crate is deliberately decoupled from the server's guard types: reasons
//! arrive as stable `&'static str` labels (see `RejectReason::label` in the
//! server), so telemetry never needs to depend on the server crate.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

/// One recorded verdict for one player on one tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    pub tick: u32,
    pub player: u8,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Outcome {
    /// Input reached the sim. `anomaly` = passed but suspicious (e.g. a move
    /// vector that had to be clamped) — a signal for the detector.
    Accepted { anomaly: bool },
    /// Input dropped by a guard.
    Rejected { reason: &'static str },
}

/// Aggregate counts, for the harness and for a quick detector baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Totals {
    pub accepted: u32,
    pub anomalies: u32,
    /// reason label -> count
    pub rejected: BTreeMap<&'static str, u32>,
}

impl Totals {
    pub fn total_rejected(&self) -> u32 {
        self.rejected.values().sum()
    }
}

#[derive(Debug, Default)]
pub struct Telemetry {
    records: Vec<Record>,
}

impl Telemetry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn accept(&mut self, tick: u32, player: u8, anomaly: bool) {
        self.records.push(Record { tick, player, outcome: Outcome::Accepted { anomaly } });
    }

    pub fn reject(&mut self, tick: u32, player: u8, reason: &'static str) {
        self.records.push(Record { tick, player, outcome: Outcome::Rejected { reason } });
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Totals across all players.
    pub fn totals(&self) -> Totals {
        self.fold(self.records.iter())
    }

    /// Totals for one player — the detector's per-player feature vector.
    pub fn per_player(&self, player: u8) -> Totals {
        self.fold(self.records.iter().filter(|r| r.player == player))
    }

    fn fold<'a>(&self, it: impl Iterator<Item = &'a Record>) -> Totals {
        let mut t = Totals::default();
        for r in it {
            match &r.outcome {
                Outcome::Accepted { anomaly } => {
                    t.accepted += 1;
                    if *anomaly {
                        t.anomalies += 1;
                    }
                }
                Outcome::Rejected { reason } => {
                    *t.rejected.entry(reason).or_insert(0) += 1;
                }
            }
        }
        t
    }

    /// Write every record as one JSON object per line (jsonl) — the format the
    /// detector trains on.
    pub fn write_jsonl<W: Write>(&self, mut w: W) -> io::Result<()> {
        for r in &self.records {
            let line = serde_json::to_string(r)?;
            writeln!(w, "{}", line)?;
        }
        Ok(())
    }

    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let f = std::fs::File::create(path)?;
        self.write_jsonl(std::io::BufWriter::new(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Telemetry {
        let mut t = Telemetry::new();
        t.accept(1, 1, false); // honest
        t.accept(1, 2, true); // cheater: clamped move flagged
        t.reject(1, 2, "rate_exceeded");
        t.reject(2, 2, "replay");
        t.reject(2, 3, "rate_exceeded");
        t
    }

    #[test]
    fn totals_count_every_outcome() {
        let t = sample();
        let tot = t.totals();
        assert_eq!(tot.accepted, 2);
        assert_eq!(tot.anomalies, 1);
        assert_eq!(tot.total_rejected(), 3);
        assert_eq!(tot.rejected.get("rate_exceeded"), Some(&2));
        assert_eq!(tot.rejected.get("replay"), Some(&1));
    }

    #[test]
    fn per_player_isolates_one_player() {
        let t = sample();
        let p2 = t.per_player(2);
        assert_eq!(p2.accepted, 1);
        assert_eq!(p2.anomalies, 1);
        assert_eq!(p2.total_rejected(), 2); // rate_exceeded + replay
        let p1 = t.per_player(1);
        assert_eq!(p1.accepted, 1);
        assert_eq!(p1.total_rejected(), 0); // honest player: nothing rejected
    }

    #[test]
    fn jsonl_has_one_line_per_record() {
        let t = sample();
        let mut buf: Vec<u8> = Vec::new();
        t.write_jsonl(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), t.len());
        assert!(lines[2].contains("rate_exceeded"));
        // each line is valid standalone JSON
        for l in lines {
            let _: serde_json::Value = serde_json::from_str(l).unwrap();
        }
    }
}
