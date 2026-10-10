//! Online detection: judge each player while they play, not after the run.
//!
//! [`Monitor::observe`] takes one telemetry [`Record`] at a time, in stream
//! order, and returns the flags that record caused. Per player it keeps two
//! views, both checked by the same [`Suite`] after every counted record:
//!
//!   - **lifetime** — everything since the player's session began. Same
//!     numbers the offline [`crate::stats`] fold produces, so at the end of a
//!     run [`Monitor::verdict`] equals [`Suite::run`] (when no id was reused).
//!   - **window** — the last [`WINDOW`] shots and the last [`WINDOW`] accepted
//!     inputs. A lifetime ratio is diluted by everything honest that came
//!     before: 300 honest shots then 60 snapped ones is 17% exact over the
//!     life (under the 25% line) and 60% over the last 100.
//!
//! A flag is raised once per (session, reason): the first record that crosses
//! a line, by either view, becomes an [`Alert`] carrying its tick. After that
//! the reviewer has the evidence; repeating it every tick adds nothing.
//!
//! `Left` ends a session: its state is dropped, so whoever is issued the id
//! next starts from zero — the offline fold merges them.
//!
//! Both views use each detector's own `MIN_*` and `THRESHOLD`, set from the
//! online peak: `aegis-harness sweep` reports, per honest player, the highest
//! value any view reached at any record once judged — judging at every record
//! from the minimum sample on is noisier than judging one whole run, and the
//! thresholds must hold against that, not against the run's final figure.

use std::collections::BTreeMap;

use aegis_telemetry::{Outcome, Record};

use crate::detectors::far_aim;
use crate::{Config, ConfigError, Flag, FlagReason, PlayerStats, Suite};

/// Samples per window: shots for the shot detectors, accepted inputs for the
/// input detectors. Large enough that every `MIN_*` (at most 60) is reachable
/// inside it; small enough that a burst of cheating is not drowned.
pub const WINDOW: usize = 100;

/// A flag, and the tick of the record that raised it.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub tick: u32,
    pub flag: Flag,
}

const _: () = assert!(WINDOW <= 128, "a window is one u128 per signal");

/// The low `WINDOW` bits.
const MASK: u128 = if WINDOW == 128 { u128::MAX } else { (1 << WINDOW) - 1 };

/// One signal over the last [`WINDOW`] samples: a shift register, newest in
/// bit 0. Pushing shifts the oldest out past bit `WINDOW - 1`.
fn push(plane: &mut u128, bit: bool) {
    *plane = ((*plane << 1) | bit as u128) & MASK;
}

/// The last [`WINDOW`] shots, accepted inputs and glimpses, one bit per
/// sample per signal: a fixed 128 bytes of planes per player however long it
/// plays. Counts are read off the planes into a [`PlayerStats`] so the
/// detectors read it like any other. Far shots are those among the last
/// [`WINDOW`] shots, not the last [`WINDOW`] far ones.
#[derive(Debug)]
struct Window {
    shots: u32,
    hit: u128,
    exact: u128,
    timed: u128,
    fast: u128,
    far: u128,
    far_inside: u128,
    inputs: u32,
    anomaly: u128,
    glimpses: u32,
    foreseen: u128,
    stats: PlayerStats,
}

impl Window {
    fn new(player: u8) -> Self {
        Self {
            shots: 0,
            hit: 0,
            exact: 0,
            timed: 0,
            fast: 0,
            far: 0,
            far_inside: 0,
            inputs: 0,
            anomaly: 0,
            glimpses: 0,
            foreseen: 0,
            stats: PlayerStats::new(player),
        }
    }

    /// [`Window::count`] at the default lines.
    #[cfg(test)]
    fn record(&mut self, o: &Outcome) {
        self.count(o, &Config::DEFAULT);
    }

    fn count(&mut self, o: &Outcome, cfg: &Config) {
        let s = &mut self.stats;
        match *o {
            Outcome::Accepted { anomaly } => {
                self.inputs = (self.inputs + 1).min(WINDOW as u32);
                push(&mut self.anomaly, anomaly);
                s.accepted = self.inputs;
                s.anomalies = self.anomaly.count_ones();
            }
            Outcome::Shot { hit, aim_err, react, size } => {
                self.shots = (self.shots + 1).min(WINDOW as u32);
                push(&mut self.hit, hit);
                push(&mut self.exact, cfg.aim_exact.is_exact(aim_err));
                push(&mut self.timed, react.is_some());
                push(&mut self.fast, react.is_some_and(|k| cfg.reaction.is_fast(k)));
                let far = cfg.far_aim.is_far(size);
                push(&mut self.far, far);
                push(&mut self.far_inside, far && far_aim::is_inside(aim_err, size));
                s.shots = self.shots;
                s.hits = self.hit.count_ones();
                s.exact = self.exact.count_ones();
                s.timed = self.timed.count_ones();
                s.fast = self.fast.count_ones();
                s.far = self.far.count_ones();
                s.far_inside = self.far_inside.count_ones();
            }
            // Foresight in bursts: a liar who foresees a few times in a row
            // and then plays straight is diluted over a life, not here.
            Outcome::Glimpse { claimed, ahead } => {
                self.glimpses = (self.glimpses + 1).min(WINDOW as u32);
                push(&mut self.foreseen, cfg.foresight.is_foreseen(claimed, ahead));
                s.glimpsed = self.glimpses;
                s.foreseen = self.foreseen.count_ones();
            }
            Outcome::Rejected { .. } | Outcome::Left => {}
        }
    }
}

/// One player's session as the monitor sees it.
#[derive(Debug)]
struct Live {
    life: PlayerStats,
    window: Window,
    /// Reasons already raised this session.
    raised: Vec<FlagReason>,
}

pub struct Monitor {
    suite: Suite,
    live: BTreeMap<u8, Live>,
}

impl Monitor {
    /// Samples are classified by the suite's lines ([`Suite::config`]).
    pub fn new(suite: Suite) -> Self {
        Self { suite, live: BTreeMap::new() }
    }

    pub fn standard() -> Self {
        Self::new(Suite::standard())
    }

    /// The standard detectors under a game's own lines; refused if
    /// [`Config::validate`] refuses them.
    pub fn with_config(cfg: Config) -> Result<Self, ConfigError> {
        Suite::with_config(cfg).map(Self::new)
    }

    /// Feed one record, in stream order. Returns the alerts it raised — empty
    /// for almost every record.
    pub fn observe(&mut self, r: &Record) -> Vec<Alert> {
        let counted = match r.outcome {
            Outcome::Left => {
                self.live.remove(&r.player);
                return Vec::new();
            }
            Outcome::Rejected { .. } => false,
            Outcome::Accepted { .. } | Outcome::Shot { .. } | Outcome::Glimpse { .. } => true,
        };
        let l = self.live.entry(r.player).or_insert_with(|| Live {
            life: PlayerStats::new(r.player),
            window: Window::new(r.player),
            raised: Vec::new(),
        });
        if !counted {
            return Vec::new();
        }
        let cfg = self.suite.config();
        l.life.count(&r.outcome, cfg);
        l.window.count(&r.outcome, cfg);
        let mut out = Vec::new();
        for flag in self.suite.check(&l.life).into_iter().chain(self.suite.check(&l.window.stats)) {
            if !l.raised.contains(&flag.reason) {
                l.raised.push(flag.reason);
                out.push(Alert { tick: r.tick, flag });
            }
        }
        out
    }

    /// Feed a whole stream; every alert it raised, in order.
    pub fn run(&mut self, records: &[Record]) -> Vec<Alert> {
        records.iter().flat_map(|r| self.observe(r)).collect()
    }

    /// Lifetime stats of the player's current session, if one is live.
    pub fn stats(&self, player: u8) -> Option<&PlayerStats> {
        self.live.get(&player).map(|l| &l.life)
    }

    /// Window stats of the player's current session, if one is live.
    pub fn window(&self, player: u8) -> Option<&PlayerStats> {
        self.live.get(&player).map(|l| &l.window.stats)
    }

    /// The lifetime verdict on every live session right now — what the
    /// offline [`Suite::run`] would say if the stream ended here.
    pub fn verdict(&self) -> Vec<Flag> {
        self.live.values().flat_map(|l| self.suite.check(&l.life)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::aim_exact::is_exact;
    use crate::detectors::reaction::is_fast;
    use crate::detectors::{accuracy, aim_exact};
    use crate::stats;
    use aegis_telemetry::Telemetry;

    const EXACT: f32 = 0.0;
    const WIDE: f32 = 0.1;

    fn reasons(alerts: &[Alert]) -> Vec<FlagReason> {
        alerts.iter().map(|a| a.flag.reason).collect()
    }

    /// `n` misses from `player` at `aim_err`, one per tick from `*tick`.
    fn shoot(t: &mut Telemetry, tick: &mut u32, player: u8, n: u32, aim_err: f32) {
        for _ in 0..n {
            t.shot(*tick, player, false, aim_err, None, 0.5);
            *tick += 1;
        }
    }

    /// `n` glimpses from `player`, foreseen or explained, one per tick.
    fn glimpse(t: &mut Telemetry, tick: &mut u32, player: u8, n: u32, foreseen: bool) {
        for _ in 0..n {
            t.glimpse(*tick, player, if foreseen { None } else { Some(0.05) }, 0.0);
            *tick += 1;
        }
    }

    /// A liar who foresees in one burst after a long straight stretch: over
    /// its life the share is under the line (3 of 403), so only the window
    /// sees it — and it does, on the third, not before.
    #[test]
    fn a_burst_of_foresight_is_seen_by_the_window_not_the_life() {
        use crate::detectors::foresight;
        let mut t = Telemetry::new();
        let mut tick = 1;
        glimpse(&mut t, &mut tick, 5, 400, false);
        glimpse(&mut t, &mut tick, 5, foresight::MIN_FORESEEN, true);
        let life = &stats(t.records())[&5];
        assert!(life.foreseen as f32 / (life.glimpsed as f32) <= foresight::THRESHOLD, "setup: life over the line");
        assert_eq!(Suite::standard().run(t.records()), vec![], "the offline fold saw it");
        let alerts = Monitor::standard().run(t.records());
        assert_eq!(reasons(&alerts), vec![FlagReason::Foresight]);
        assert_eq!(alerts[0].tick, tick - 1, "raised on the last of the burst");
        assert_eq!(alerts[0].flag.samples, WINDOW as u32);
    }

    /// The window forgets: the same 3, spread one per 100 glimpses, never
    /// sit in one window together and never cross the life's share.
    #[test]
    fn foresight_spread_thin_is_never_flagged() {
        let mut t = Telemetry::new();
        let mut tick = 1;
        for _ in 0..10 {
            glimpse(&mut t, &mut tick, 5, 1, true);
            glimpse(&mut t, &mut tick, 5, WINDOW as u32, false);
        }
        assert_eq!(Monitor::standard().run(t.records()), vec![]);
    }

    /// A game's lines change what a sample is, not only where the share is
    /// cut: at 60 Hz a 5-tick reaction (83 ms) is fast; at 30 Hz (167 ms)
    /// it is not.
    #[test]
    fn a_games_tick_rate_changes_what_reads_fast() {
        use crate::detectors::reaction::MIN_TIMED;
        let mut t = Telemetry::new();
        for k in 0..MIN_TIMED {
            t.shot(k, 6, false, WIDE, Some(5), 0.5);
        }
        assert_eq!(Monitor::standard().run(t.records()), vec![]);
        let at60 = Monitor::with_config(Config::at_tick_rate(60)).unwrap().run(t.records());
        assert_eq!(reasons(&at60), vec![FlagReason::Reaction]);
    }

    #[test]
    fn a_config_that_cannot_mean_what_it_says_is_refused() {
        let mut c = Config::DEFAULT;
        c.foresight.clear_rad = c.foresight.fit_rad / 2.0;
        assert_eq!(Monitor::with_config(c).err().map(|e| e.field), Some("foresight.clear_rad"));
    }

    #[test]
    fn left_starts_a_new_person_where_the_offline_fold_merges_them() {
        // An aimbot holds id 1, leaves; an honest player is issued id 1 next.
        let mut t = Telemetry::new();
        let mut tick = 1;
        shoot(&mut t, &mut tick, 1, 40, EXACT);
        t.left(tick, 1);
        shoot(&mut t, &mut tick, 1, 40, WIDE);

        // v0 (offline) blames the newcomer for the predecessor's aim: 50% exact.
        let v0 = Suite::standard().run(t.records());
        assert_eq!(v0.iter().map(|f| f.reason).collect::<Vec<_>>(), vec![FlagReason::AimExact]);

        let mut m = Monitor::standard();
        let alerts = m.run(t.records());
        // The aimbot was caught while it played, at its MIN_SHOTS-th shot ...
        assert_eq!(reasons(&alerts), vec![FlagReason::AimExact]);
        assert_eq!(alerts[0].tick, aim_exact::MIN_SHOTS);
        // ... and the newcomer is judged on their own 40 shots: clean.
        assert_eq!(m.stats(1).map(|s| (s.shots, s.exact)), Some((40, 0)));
        assert_eq!(m.verdict(), vec![]);
    }

    #[test]
    fn a_flag_is_raised_once_per_session_at_the_crossing_record() {
        let mut t = Telemetry::new();
        let mut tick = 1;
        shoot(&mut t, &mut tick, 4, 500, EXACT);
        let mut m = Monitor::standard();
        let alerts = m.run(t.records());
        assert_eq!(reasons(&alerts), vec![FlagReason::AimExact]);
        assert_eq!((alerts[0].tick, alerts[0].flag.samples), (aim_exact::MIN_SHOTS, aim_exact::MIN_SHOTS));

        // A new session under the same id is judged — and flagged — afresh.
        let mut t2 = Telemetry::new();
        t2.left(tick, 4);
        shoot(&mut t2, &mut tick, 4, aim_exact::MIN_SHOTS, EXACT);
        assert_eq!(reasons(&m.run(t2.records())), vec![FlagReason::AimExact]);
    }

    #[test]
    fn rejected_inputs_are_not_counted_and_raise_nothing() {
        let mut t = Telemetry::new();
        for tick in 0..200 {
            t.reject(tick, 2, "rate_exceeded");
        }
        let mut m = Monitor::standard();
        assert_eq!(m.run(t.records()), vec![]);
        assert_eq!(m.stats(2), Some(&PlayerStats::new(2)));
    }

    #[test]
    fn a_burst_the_lifetime_dilutes_is_caught_by_the_window() {
        // 300 honest-looking shots, then the aimbot is switched on for 60.
        let mut t = Telemetry::new();
        let mut tick = 1;
        shoot(&mut t, &mut tick, 9, 300, WIDE);
        shoot(&mut t, &mut tick, 9, 60, EXACT);

        // Lifetime: 60/360 = 17% exact, under the line. v0 never flags it.
        assert_eq!(Suite::standard().run(t.records()), vec![]);

        let mut m = Monitor::standard();
        let alerts = m.run(t.records());
        assert_eq!(m.verdict(), vec![]); // the lifetime view agrees with v0
        assert_eq!(reasons(&alerts), vec![FlagReason::AimExact]);
        let a = &alerts[0];
        // The window catches it at the first snapped shot that takes the last
        // 100 strictly over the line.
        let needed = (aim_exact::THRESHOLD * WINDOW as f32) as u32 + 1;
        assert_eq!(a.tick, 1 + 300 + needed - 1);
        assert_eq!((a.flag.samples, a.flag.value), (WINDOW as u32, needed as f32 / WINDOW as f32));
    }

    #[test]
    fn lifetime_matches_the_offline_fold_record_for_record() {
        // Oracle: on a stream with no reused id, the monitor's lifetime stats
        // after every record equal v0's fold of the prefix, and its final
        // verdict equals v0's.
        let recs = random_stream(7, 3000, false);
        let mut m = Monitor::standard();
        for (i, r) in recs.iter().enumerate() {
            m.observe(r);
            if i % 97 == 0 || i + 1 == recs.len() {
                for (p, s) in stats(&recs[..=i]) {
                    assert_eq!(m.stats(p), Some(&s), "player {p} after record {i}");
                }
            }
        }
        assert_eq!(m.verdict(), Suite::standard().run(&recs));
    }

    /// The window as it was before the bit planes (2026-10-08): a deque of
    /// samples with running counts. Kept as the rewrite's oracle.
    struct DequeWindow {
        shots: std::collections::VecDeque<(bool, bool, bool, bool, bool, bool)>,
        inputs: std::collections::VecDeque<bool>,
        stats: PlayerStats,
    }

    impl DequeWindow {
        fn new(player: u8) -> Self {
            Self { shots: Default::default(), inputs: Default::default(), stats: PlayerStats::new(player) }
        }

        fn record(&mut self, o: &Outcome) {
            let s = &mut self.stats;
            match *o {
                Outcome::Accepted { anomaly } => {
                    if self.inputs.len() == WINDOW {
                        let old = self.inputs.pop_front().expect("full window");
                        s.accepted -= 1;
                        s.anomalies -= old as u32;
                    }
                    self.inputs.push_back(anomaly);
                    s.accepted += 1;
                    s.anomalies += anomaly as u32;
                }
                Outcome::Shot { hit, aim_err, react, size } => {
                    if self.shots.len() == WINDOW {
                        let (h, e, t, f, r, i) = self.shots.pop_front().expect("full window");
                        s.shots -= 1;
                        s.hits -= h as u32;
                        s.exact -= e as u32;
                        s.timed -= t as u32;
                        s.fast -= f as u32;
                        s.far -= r as u32;
                        s.far_inside -= i as u32;
                    }
                    let (e, t, f) = (is_exact(aim_err), react.is_some(), react.is_some_and(is_fast));
                    let r = size < far_aim::FAR_RAD;
                    let i = r && aim_err <= size;
                    self.shots.push_back((hit, e, t, f, r, i));
                    s.shots += 1;
                    s.hits += hit as u32;
                    s.exact += e as u32;
                    s.timed += t as u32;
                    s.fast += f as u32;
                    s.far += r as u32;
                    s.far_inside += i as u32;
                }
                Outcome::Glimpse { .. } | Outcome::Rejected { .. } | Outcome::Left => {}
            }
        }
    }

    /// Rewrite check: the bit-plane window equals the deque it replaced
    /// after every record, sessions reset on `Left`, across seeds — and the
    /// streams are long enough that every plane has wrapped many times.
    #[test]
    fn bit_planes_match_the_deque_they_replaced() {
        for seed in 1..=8 {
            let recs = random_stream(seed, 6000, true);
            let mut new: BTreeMap<u8, Window> = BTreeMap::new();
            let mut old: BTreeMap<u8, DequeWindow> = BTreeMap::new();
            let mut full = 0;
            for (i, r) in recs.iter().enumerate() {
                let p = r.player;
                if r.outcome == Outcome::Left {
                    new.remove(&p);
                    old.remove(&p);
                    continue;
                }
                let n = new.entry(p).or_insert_with(|| Window::new(p));
                let o = old.entry(p).or_insert_with(|| DequeWindow::new(p));
                n.record(&r.outcome);
                o.record(&r.outcome);
                assert_eq!(n.stats, o.stats, "seed {seed}, record {i}");
                full += (o.shots.len() == WINDOW) as u32;
            }
            assert!(full > 200, "seed {seed}: the window was full only {full} times");
        }
    }

    #[test]
    fn window_matches_a_naive_recount_of_the_last_samples() {
        // Oracle: recount the window from scratch — the last WINDOW shots and
        // inputs since the player's last Left — after every record.
        for seed in 1..=5 {
            let recs = random_stream(seed, 4000, true);
            let mut m = Monitor::standard();
            for (i, r) in recs.iter().enumerate() {
                m.observe(r);
                let p = r.player;
                let naive = naive_window(&recs[..=i], p);
                assert_eq!(m.window(p).cloned(), naive, "seed {seed}, player {p}, record {i}");
            }
        }
    }

    #[test]
    fn no_view_judges_below_its_detectors_minimum() {
        // Every shot a hit, none exact: accuracy alone is judged, at exactly
        // its MIN_SHOTS-th shot and not one before. (Out of the standard
        // suite, but the monitor must still honour its minimum.)
        let mut m = Monitor::new(Suite::new(vec![
            Box::new(accuracy::AccuracyDetector::default()),
            Box::new(aim_exact::AimExactDetector::default()),
        ]));
        for n in 1..=accuracy::MIN_SHOTS {
            let a = m.observe(&Record {
                tick: n,
                player: 5,
                outcome: Outcome::Shot { hit: true, aim_err: WIDE, react: None, size: 0.5 },
            });
            assert_eq!(
                reasons(&a),
                if n == accuracy::MIN_SHOTS { vec![FlagReason::Accuracy] } else { vec![] },
                "shot {n}"
            );
        }
        // Every shot exact, none a hit: aim_exact alone, at its own minimum.
        let mut m = Monitor::standard();
        for n in 1..=aim_exact::MIN_SHOTS {
            let a = m.observe(&Record {
                tick: n,
                player: 5,
                outcome: Outcome::Shot { hit: false, aim_err: EXACT, react: None, size: 0.5 },
            });
            assert_eq!(
                reasons(&a),
                if n == aim_exact::MIN_SHOTS { vec![FlagReason::AimExact] } else { vec![] },
                "shot {n}"
            );
        }
    }

    fn naive_window(recs: &[Record], p: u8) -> Option<PlayerStats> {
        let start = recs.iter().rposition(|r| r.player == p && r.outcome == Outcome::Left).map_or(0, |i| i + 1);
        let mine: Vec<&Record> = recs[start..].iter().filter(|r| r.player == p).collect();
        if mine.is_empty() {
            return None;
        }
        let shots: Vec<&Record> = mine.iter().copied().filter(|r| matches!(r.outcome, Outcome::Shot { .. })).collect();
        let inputs: Vec<&Record> =
            mine.iter().copied().filter(|r| matches!(r.outcome, Outcome::Accepted { .. })).collect();
        let mut s = PlayerStats::new(p);
        for r in shots.iter().rev().take(WINDOW).chain(inputs.iter().rev().take(WINDOW)) {
            s.record(&r.outcome);
        }
        Some(s)
    }

    /// A deterministic mixed stream over 4 players: accepts (some anomalous),
    /// rejects, shots (some exact, some hits, some timed), and — if `leaves`
    /// — the odd `Left`.
    fn random_stream(seed: u64, n: usize, leaves: bool) -> Vec<Record> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut t = Telemetry::new();
        for i in 0..n {
            let tick = i as u32 / 4;
            let p = (next() % 4) as u8 + 1;
            match next() % 100 {
                0 if leaves => t.left(tick, p),
                0..=39 => t.accept(tick, p, next() % 5 == 0),
                40..=49 => t.reject(tick, p, "replay"),
                _ => {
                    // Untimed mostly; timed ones from instant to slow.
                    let react = (next() % 3 == 0).then(|| (next() % 10) as u32);
                    // Far (inside or not, by the aim) and near targets.
                    let size = [0.02, 0.03, 0.06, 0.5][(next() % 4) as usize];
                    t.shot(tick, p, next() % 3 == 0, if next() % 4 == 0 { 0.0 } else { 0.05 }, react, size)
                }
            }
        }
        t.records().to_vec()
    }
}
