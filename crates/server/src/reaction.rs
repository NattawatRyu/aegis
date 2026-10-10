//! How long a shooter took to fire at an enemy that came into sight — the
//! evidence a reaction-time detector reads (`Outcome::Shot::react`).
//!
//! An *engagement* is shooter S and enemy E, from the tick E enters S's
//! sight until E leaves it. Its first evidence shot carries
//! `react = seen - since`, where `seen` is the snapshot S's input says it
//! was chosen on (proven: `guards::tick_proof`): 0 means S fired on the
//! very snapshot that first showed E, however long either leg took. Later
//! shots in the same engagement carry `None`.
//!
//! Everything here runs in the shooter's ticks, the snapshots it acted on:
//! the enemy a shot is at comes from that snapshot's frame
//! (`crate::history`), its run of fire counts those ticks, and an
//! engagement is a sight interval, so a shot chosen while E was in sight
//! still counts if it arrives after E left. Timed from arrival instead
//! (`tick - since`, until 2026-10-08), a client acting on an old picture had
//! its shot scored against whoever the server saw nearest *now* — often an
//! enemy that had just come into sight — and read as an instant reaction:
//! at a 6-tick round trip, 14 of 24 honest walkers were flagged.
//!
//! In sight means: S alive, E alive, and S [`Visibility::sees`] E. So a
//! respawn — either side — starts a new engagement, and a corpse never
//! starts one.
//!
//! Only a shot that starts firing is timed: the first shot at E must also be
//! the first of S's unbroken run of firing (one tick without a shot ends a
//! run). Otherwise it carries `None`, for either of two reasons:
//!
//!   - *prefire* — S was already firing before E appeared, spraying a corner;
//!     its first shot at E says nothing about how fast it saw E.
//!   - *a target switch* — S was busy firing at another enemy when E's turn
//!     came; the wait measures the fight it was in, not its reflexes. (Traced
//!     on the triggerbot, 2026-10-08: respawned facing six, it worked through
//!     them nearest-first and the switches read 3, 4, 5 ticks.) Busy is not
//!     slow, and not fast either.
//!
//! Server-side only, from authoritative sight and accepted inputs — nothing
//! the client sends can set it. Fixed size per room (~530 KB): nothing is
//! allocated per tick.

use aegis_protocol::PlayerId;

use crate::{Visibility, World};

const WORDS: usize = 4;
type Row = [u64; WORDS];

fn bit(id: PlayerId) -> (usize, u64) {
    (id as usize / 64, 1u64 << (id as usize % 64))
}

fn has(r: &Row, id: PlayerId) -> bool {
    let (w, m) = bit(id);
    r[w] & m != 0
}

pub struct Reaction {
    /// Who each shooter had in sight at the last update.
    seen: Box<[Row; 256]>,
    /// `since[s][e]`: the tick `e` last came into `s`'s sight, starting its
    /// latest sight interval. `NONE` for a pair with no interval (one side
    /// is a newcomer whose sight starts next tick).
    since: Box<[[u32; 256]; 256]>,
    /// `until[s][e]`: the tick that latest interval ended — `e` no longer in
    /// sight. Read only while `seen[s]` lacks `e`.
    until: Box<[[u32; 256]; 256]>,
    /// Enemies `s` has already put on record in their latest interval.
    /// Cleared when a new one starts, not when it ends: a shot chosen on a
    /// snapshot from before `e` left can still arrive after.
    engaged: Box<[Row; 256]>,
    /// The snapshot tick `s` last fired on, and the first of its unbroken run
    /// of firing — both in the shooter's ticks, the snapshots it acted on.
    last_shot: [Option<u32>; 256],
    streak_start: [u32; 256],
}

/// `since` for a pair that has no sight interval at all.
const NONE: u32 = u32::MAX;

impl Default for Reaction {
    fn default() -> Self {
        Self {
            seen: Box::new([[0; WORDS]; 256]),
            since: Box::new([[NONE; 256]; 256]),
            until: Box::new([[0; 256]; 256]),
            engaged: Box::new([[0; WORDS]; 256]),
            last_shot: [None; 256],
            streak_start: [0; 256],
        }
    }
}

/// Bitset of the living.
fn alive(sim: &impl World) -> Row {
    let mut r = [0; WORDS];
    for p in sim.players().iter().filter(|p| p.alive) {
        let (w, m) = bit(p.id);
        r[w] |= m;
    }
    r
}

impl Reaction {
    /// Start of tick `tick`, after `vis` was computed on `sim`: every pair
    /// newly in sight starts a sight interval now; every pair no longer in
    /// sight ends one.
    pub fn observe(&mut self, tick: u32, sim: &impl World, vis: &Visibility) {
        let alive = alive(sim);
        for s in 0..=PlayerId::MAX {
            let si = s as usize;
            let now = if has(&alive, s) {
                let row = vis.row(s);
                std::array::from_fn(|w| row[w] & alive[w])
            } else {
                [0; WORDS]
            };
            let before = self.seen[si];
            for w in 0..WORDS {
                let mut new = now[w] & !before[w];
                let mut gone = before[w] & !now[w];
                self.engaged[si][w] &= !new;
                while new != 0 {
                    self.since[si][w * 64 + new.trailing_zeros() as usize] = tick;
                    new &= new - 1;
                }
                while gone != 0 {
                    self.until[si][w * 64 + gone.trailing_zeros() as usize] = tick;
                    gone &= gone - 1;
                }
            }
            self.seen[si] = now;
        }
    }

    /// Player `id` was admitted mid-tick (already in `sim` and `vis`): a new
    /// person, whatever the id held before. This tick's snapshots have gone
    /// out without it, so everything in sight between it and anyone else
    /// starts with the next one, `tick + 1`; whatever its predecessor saw or
    /// was seen by is gone.
    pub fn joined(&mut self, tick: u32, sim: &impl World, vis: &Visibility, id: PlayerId) {
        let tick = tick.wrapping_add(1);
        let alive = alive(sim);
        let (i, (w, m)) = (id as usize, bit(id));
        self.last_shot[i] = None;
        self.seen[i] = [0; WORDS];
        self.engaged[i] = [0; WORDS];
        self.since[i] = [NONE; 256];
        for s in 0..256 {
            self.seen[s][w] &= !m;
            self.engaged[s][w] &= !m;
            self.since[s][i] = NONE;
        }
        if !has(&alive, id) {
            return;
        }
        for e in sim.players().iter().filter(|p| p.alive && p.id != id).map(|p| p.id) {
            if vis.sees(id, e) {
                let (ew, em) = bit(e);
                self.seen[i][ew] |= em;
                self.since[i][e as usize] = tick;
            }
            if vis.sees(e, id) {
                self.seen[e as usize][w] |= m;
                self.since[e as usize][i] = tick;
            }
        }
    }

    /// `s` fired on the snapshot of tick `seen` (any shot, evidence or not):
    /// extends or starts its run of fire. Runs are counted in the shooter's
    /// own ticks: a client a round trip behind fires on the same snapshots,
    /// in the same rhythm, as one on the server's doorstep. `seen` never goes
    /// back (`guards::stale_tick`). Call before [`Reaction::engage`] for the
    /// same shot.
    ///
    /// A snapshot the client never got (lost on the way) is a tick it did
    /// not fire on, so it ends the run — on a lossy link a run can read as
    /// two, and the second one's first shot as a start.
    pub fn fired(&mut self, seen: u32, s: PlayerId) {
        let i = s as usize;
        if !matches!(self.last_shot[i], Some(t) if t == seen || t.wrapping_add(1) == seen) {
            self.streak_start[i] = seen;
        }
        self.last_shot[i] = Some(seen);
    }

    /// `s` put a shot on record against `e`, choosing it on the snapshot of
    /// `seen` (the tick its input proved). Its reaction time, `seen - since`,
    /// if this is the first shot in that sight interval and the first of its
    /// run of firing (not prefire, not a target switch), else `None`.
    ///
    /// `None` too, without using the engagement up, if `e` was not in `s`'s
    /// sight on `seen` as tracked here — before its latest interval began
    /// (an earlier interval is not remembered), or after it ended. The
    /// evidence target comes from the frame of `seen` (`crate::history`),
    /// so it always should be in sight there.
    ///
    /// Timed from `seen`, not from when the shot arrived, so latency is not
    /// in it: a client a round trip behind that fires on the snapshot
    /// showing `e` reads 0, the same as one on the server's doorstep.
    pub fn engage(&mut self, s: PlayerId, e: PlayerId, seen: u32) -> Option<u32> {
        let (si, ei, (w, m)) = (s as usize, e as usize, bit(e));
        let since = self.since[si][ei];
        let ongoing = self.seen[si][w] & m != 0;
        if since == NONE || seen < since || (!ongoing && seen >= self.until[si][ei]) {
            return None;
        }
        if self.engaged[si][w] & m != 0 {
            return None;
        }
        self.engaged[si][w] |= m;
        (self.streak_start[si] == seen).then_some(seen - since)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ARENA_HALF, ARENA_WALLS, HIT_RADIUS, MAX_HEALTH, SHOT_DAMAGE};
    use crate::{Sim, Wall};
    use aegis_protocol::Vec2;
    use std::collections::{HashMap, HashSet};

    /// A 2x2 box centred on (5,0): S at the origin cannot see (10,0), can
    /// see (10,5).
    fn pillar() -> Sim {
        Sim::with_walls(&[Wall::new(Vec2::new(4.0, -1.0), Vec2::new(6.0, 1.0))])
    }

    const HIDDEN: Vec2 = Vec2 { x: 10.0, y: 0.0 };
    const UP: Vec2 = Vec2 { x: 0.0, y: 1.0 };
    const DOWN: Vec2 = Vec2 { x: 0.0, y: -1.0 };

    fn begin(r: &mut Reaction, sim: &Sim, t: u32) -> Visibility {
        let v = Visibility::compute(sim);
        r.observe(t, sim, &v);
        v
    }

    fn shoot(r: &mut Reaction, t: u32, s: PlayerId, e: PlayerId) -> Option<u32> {
        r.fired(t, s);
        r.engage(s, e, t)
    }

    /// Players 1 at the origin and 2 behind the pillar.
    fn hidden_pair() -> Sim {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, HIDDEN);
        sim
    }

    #[test]
    fn instant_is_zero_and_only_the_first_shot_counts() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, HIDDEN);
        let mut r = Reaction::default();
        begin(&mut r, &sim, 1);
        assert_eq!(shoot(&mut r, 1, 1, 2), Some(0));
        begin(&mut r, &sim, 2);
        assert_eq!(shoot(&mut r, 2, 1, 2), None, "second shot of the engagement");
    }

    #[test]
    fn delay_counts_ticks_since_coming_into_sight() {
        let mut sim = hidden_pair();
        let mut r = Reaction::default();
        begin(&mut r, &sim, 1);
        sim.apply_move(2, UP); // (10, 5): in the open
        for t in 2..=5 {
            begin(&mut r, &sim, t);
        }
        assert_eq!(shoot(&mut r, 5, 1, 2), Some(3));
    }

    #[test]
    fn leaving_sight_ends_the_engagement() {
        let mut sim = hidden_pair();
        sim.apply_move(2, UP);
        let mut r = Reaction::default();
        begin(&mut r, &sim, 1);
        assert_eq!(shoot(&mut r, 1, 1, 2), Some(0));
        sim.apply_move(2, DOWN); // back behind the box
        begin(&mut r, &sim, 2);
        begin(&mut r, &sim, 3);
        sim.apply_move(2, UP);
        begin(&mut r, &sim, 4);
        begin(&mut r, &sim, 5);
        assert_eq!(shoot(&mut r, 5, 1, 2), Some(1), "back in sight is a new engagement");
    }

    /// Edge: firing every tick since before the enemy appeared is prefire —
    /// starting on the very tick it appears is not, and one tick off the
    /// trigger ends the streak.
    #[test]
    fn prefire_is_none_and_a_one_tick_gap_breaks_it() {
        let run = |fire: &[u32]| {
            let mut sim = hidden_pair();
            let mut r = Reaction::default();
            let mut out = None;
            for t in 1..=6 {
                if t == 4 {
                    sim.apply_move(2, UP);
                }
                begin(&mut r, &sim, t);
                if fire.contains(&t) {
                    r.fired(t, 1);
                    if t >= 4 {
                        out = r.engage(1, 2, t);
                        break;
                    }
                }
            }
            out
        };
        // 2 appears on tick 4 (the move happens before tick 4's begin).
        assert_eq!(run(&[1, 2, 3, 4]), None, "sprayed through the corner");
        assert_eq!(run(&[3, 4]), None, "one tick of prefire is still prefire");
        assert_eq!(run(&[4]), Some(0), "first shot on the tick it appeared");
        assert_eq!(run(&[1, 2, 5]), Some(1), "gap on 3 and 4 ended the streak");
        assert_eq!(run(&[1, 2, 4]), Some(0), "gap on 3 ended the streak");
    }

    /// Edge: busy on one enemy when another's turn comes is not a reaction.
    /// 1 fires at 2 from tick 1; 3 has been in sight since tick 1 too; on
    /// tick 4 1 switches to 3 without a break — `None`, though both appeared
    /// together. After a one-tick break it is timed again.
    #[test]
    fn a_target_switch_is_not_timed() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        sim.spawn(3, Vec2::new(0.0, 20.0));
        sim.spawn(4, Vec2::new(-30.0, 0.0));
        let mut r = Reaction::default();
        for t in 1..=3 {
            begin(&mut r, &sim, t);
            let got = shoot(&mut r, t, 1, 2);
            assert_eq!(got, (t == 1).then_some(0), "tick {t}");
        }
        begin(&mut r, &sim, 4);
        assert_eq!(shoot(&mut r, 4, 1, 3), None, "a switch was timed");
        begin(&mut r, &sim, 5);
        begin(&mut r, &sim, 6); // 1 holds fire on 5
        assert_eq!(shoot(&mut r, 6, 1, 4), Some(5), "from rest after a break");
    }

    /// Edge: timed from the snapshot the shooter acted on, not from when its
    /// shot arrived. 2 comes into sight on tick 4; by tick 7, when 1's shot
    /// arrives a round trip later, it is all the server has seen. Chosen on
    /// snapshot 4: 0. Chosen on 3, which 2 was not in: `None`, and the
    /// engagement stays open — 1's next run of fire, chosen on 6 after
    /// holding fire on 4 and 5, reads 2.
    #[test]
    fn timed_from_the_snapshot_acted_on() {
        let world = || {
            let mut sim = hidden_pair();
            let mut r = Reaction::default();
            for t in 1..=3 {
                begin(&mut r, &sim, t);
            }
            sim.apply_move(2, UP);
            for t in 4..=7 {
                begin(&mut r, &sim, t);
            }
            r
        };
        let mut r = world();
        r.fired(4, 1);
        assert_eq!(r.engage(1, 2, 4), Some(0), "a round trip read as reaction");
        let mut r = world();
        r.fired(3, 1);
        assert_eq!(r.engage(1, 2, 3), None, "timed against a picture 2 was not in");
        r.fired(6, 1);
        assert_eq!(r.engage(1, 2, 6), Some(2), "the blind shot used up the engagement");
    }

    /// Edge: 2 is in sight on ticks 4..=5 and gone from 6. A shot chosen on
    /// 5 that arrives after 2 has left is still the reaction it was: 1.
    /// Chosen on 6, 2 was not in the picture: `None`. And once 2 comes back
    /// (a new interval, from 9), a late shot chosen in the old one is
    /// `None` — only the latest interval is remembered — and does not use
    /// the new one up.
    #[test]
    fn a_shot_chosen_before_the_enemy_left_still_counts() {
        let world = || {
            let mut sim = hidden_pair();
            let mut r = Reaction::default();
            begin(&mut r, &sim, 3);
            sim.apply_move(2, UP);
            begin(&mut r, &sim, 4);
            begin(&mut r, &sim, 5);
            sim.apply_move(2, DOWN);
            for t in 6..=8 {
                begin(&mut r, &sim, t);
            }
            (sim, r)
        };
        let (_, mut r) = world();
        r.fired(5, 1);
        assert_eq!(r.engage(1, 2, 5), Some(1), "dropped because 2 left before it arrived");
        let (_, mut r) = world();
        r.fired(6, 1);
        assert_eq!(r.engage(1, 2, 6), None, "timed though 2 had left the picture");
        let (mut sim, mut r) = world();
        sim.apply_move(2, UP);
        begin(&mut r, &sim, 9);
        r.fired(5, 1);
        assert_eq!(r.engage(1, 2, 5), None, "timed in an interval since replaced");
        r.fired(9, 1);
        assert_eq!(r.engage(1, 2, 9), Some(0), "the late shot used up the new interval");
    }

    fn kill(sim: &mut Sim, shooter: PlayerId, aim: Vec2) {
        for _ in 0..(MAX_HEALTH / SHOT_DAMAGE) {
            sim.apply_shot(shooter, aim);
        }
    }

    /// A respawn starts a new engagement on both sides; the dead see nothing
    /// and are seen by nobody.
    #[test]
    fn death_and_respawn_restart_engagements() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, HIDDEN);
        sim.spawn(3, Vec2::new(0.0, 10.0));
        let mut r = Reaction::default();
        begin(&mut r, &sim, 1);
        assert_eq!(shoot(&mut r, 1, 1, 2), Some(0));
        assert_eq!(shoot(&mut r, 1, 3, 2), Some(0));
        kill(&mut sim, 1, Vec2::new(1.0, 0.0));
        begin(&mut r, &sim, 2);
        assert_eq!(r.row_of(2), [0; WORDS], "a corpse sees");
        assert!(!has(&r.row_of(1), 2) && !has(&r.row_of(3), 2), "a corpse is seen");
        let mut t = 2;
        while !sim.player(2).unwrap().alive {
            t += 1;
            sim.step_respawns();
            begin(&mut r, &sim, t);
        }
        begin(&mut r, &sim, t + 1);
        begin(&mut r, &sim, t + 2);
        assert_eq!(shoot(&mut r, t + 2, 1, 2), Some(2), "respawned enemy is a new engagement");
        assert_eq!(shoot(&mut r, t + 2, 2, 3), Some(2), "respawned shooter sees afresh");
    }

    /// Edge: a mid-tick join has been in no snapshot yet; it is first shown
    /// on the next tick. A blind shot at it before then times nothing and
    /// does not use up the engagement.
    #[test]
    fn a_mid_tick_join_is_timed_from_the_next_snapshot() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        let mut r = Reaction::default();
        let mut v = begin(&mut r, &sim, 5);
        sim.spawn(2, HIDDEN);
        v.add(&sim, 2);
        r.joined(5, &sim, &v, 2);
        assert_eq!(shoot(&mut r, 5, 1, 2), None, "blind shot timed");
        begin(&mut r, &sim, 6);
        begin(&mut r, &sim, 7);
        assert_eq!(shoot(&mut r, 7, 1, 2), Some(1));
        assert_eq!(shoot(&mut r, 7, 2, 1), Some(1), "the joiner's own sight starts next tick too");
    }

    /// Edge: an id handed to a new person (old one left, new one joined) is
    /// a stranger — the old engagement and streak do not carry over. Uses the
    /// last word's last id.
    #[test]
    fn a_reused_id_starts_from_nothing() {
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(255, HIDDEN);
        let mut r = Reaction::default();
        begin(&mut r, &sim, 1);
        assert_eq!(shoot(&mut r, 1, 1, 255), Some(0));
        assert_eq!(shoot(&mut r, 1, 255, 1), Some(0));
        // The old 255 keeps firing through tick 3, then leaves; a new person
        // gets 255 on tick 3 and is first shown on tick 4.
        for t in 2..=3 {
            begin(&mut r, &sim, t);
            r.fired(t, 255);
        }
        sim.despawn(255);
        sim.spawn(255, HIDDEN);
        let v = Visibility::compute(&sim);
        r.joined(3, &sim, &v, 255);
        begin(&mut r, &sim, 4);
        assert_eq!(shoot(&mut r, 4, 1, 255), Some(0), "old engagement carried to the new 255");
        assert_eq!(shoot(&mut r, 4, 255, 1), Some(0), "old streak carried to the new 255");
    }

    impl Reaction {
        fn row_of(&self, s: PlayerId) -> Row {
            self.seen[s as usize]
        }
    }

    /// Sight intervals, oldest first: (since, until).
    type Intervals = Vec<(u32, Option<u32>)>;

    /// The same rules, written the slow obvious way: sight from `Sim::sees`
    /// every time, maps instead of bits, and every sight interval a pair
    /// ever had kept — the tracker keeps only the latest, so a shot whose
    /// snapshot falls in an earlier one must read `None` here too.
    #[derive(Default)]
    struct Naive {
        /// Every sight interval per pair, oldest first: (since, until).
        iv: HashMap<(PlayerId, PlayerId), Intervals>,
        /// (shooter, enemy, interval start) put on record.
        engaged: HashSet<(PlayerId, PlayerId, u32)>,
        last: HashMap<PlayerId, u32>,
        start: HashMap<PlayerId, u32>,
    }

    fn in_sight(sim: &Sim) -> HashSet<(PlayerId, PlayerId)> {
        let ps = sim.players();
        let mut out = HashSet::new();
        for a in ps.iter().filter(|p| p.alive) {
            for b in ps.iter().filter(|p| p.alive && p.id != a.id) {
                if sim.sees(a.pos, b.pos) {
                    out.insert((a.id, b.id));
                }
            }
        }
        out
    }

    impl Naive {
        fn open(&self, k: (PlayerId, PlayerId)) -> bool {
            self.iv.get(&k).and_then(|v| v.last()).is_some_and(|&(_, until)| until.is_none())
        }

        fn observe(&mut self, t: u32, sim: &Sim) {
            let now = in_sight(sim);
            for (k, v) in self.iv.iter_mut() {
                if let Some(last) = v.last_mut().filter(|(_, u)| u.is_none() && !now.contains(k)) {
                    last.1 = Some(t);
                }
            }
            for k in now {
                if !self.open(k) {
                    self.iv.entry(k).or_default().push((t, None));
                }
            }
        }

        fn joined(&mut self, t: u32, sim: &Sim, id: PlayerId) {
            self.iv.retain(|&(a, b), _| a != id && b != id);
            self.engaged.retain(|&(a, b, _)| a != id && b != id);
            self.last.remove(&id);
            for k in in_sight(sim).into_iter().filter(|&(a, b)| a == id || b == id) {
                self.iv.insert(k, vec![(t + 1, None)]);
            }
        }

        fn fired(&mut self, seen: u32, s: PlayerId) {
            if !matches!(self.last.get(&s), Some(&l) if l == seen || l + 1 == seen) {
                self.start.insert(s, seen);
            }
            self.last.insert(s, seen);
        }

        /// The interval `seen` falls in, if any, and whether it is the
        /// pair's latest.
        fn interval(&self, s: PlayerId, e: PlayerId, seen: u32) -> Option<(u32, bool)> {
            let v = self.iv.get(&(s, e))?;
            let i = v.iter().position(|&(since, until)| since <= seen && until.is_none_or(|u| seen < u))?;
            Some((v[i].0, i + 1 == v.len()))
        }

        fn engage(&mut self, s: PlayerId, e: PlayerId, seen: u32) -> Option<u32> {
            let (since, latest) = self.interval(s, e, seen)?;
            if !latest || !self.engaged.insert((s, e, since)) {
                return None;
            }
            (self.start[&s] == seen).then(|| seen - since)
        }
    }

    /// Nearest enemy `s` sees among those alive at the start of the tick
    /// (`shown`), beyond point-blank, with `s` alive now: the aim-evidence
    /// target, found without `Visibility`.
    fn naive_target(sim: &Sim, shown: &HashSet<PlayerId>, s: PlayerId) -> Option<PlayerId> {
        let me = sim.player(s).filter(|p| p.alive)?;
        let d2 = |p: Vec2| (p.x - me.pos.x).powi(2) + (p.y - me.pos.y).powi(2);
        let e = sim
            .players()
            .iter()
            .filter(|p| p.id != s && shown.contains(&p.id) && sim.sees(me.pos, p.pos))
            .min_by(|a, b| d2(a.pos).total_cmp(&d2(b.pos)))?;
        (d2(e.pos) > HIT_RADIUS * HIT_RADIUS).then_some(e.id)
    }

    /// The oracle: a random walled world with joins on reused and high ids,
    /// leaves, kills and respawns, every shooter acting up to 3 ticks
    /// behind (never going back, as `stale_tick` enforces); every shot's
    /// reaction must match the naive version exactly — at the evidence
    /// target, and sometimes at any other player, so that a target out of
    /// sight, left, or seen only in an earlier interval are all exercised.
    /// Also checks the run hit every branch.
    #[test]
    fn matches_the_naive_rules_in_a_random_world() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut instant, mut timed, mut none, mut shots, mut fallen) = (0, 0, 0, 0, 0);
        let (mut unseen, mut after_left, mut earlier) = (0, 0, 0);
        for _world in 0..4 {
            let mut sim = Sim::with_walls(&ARENA_WALLS);
            let (mut r, mut n) = (Reaction::default(), Naive::default());
            let mut vis = Visibility::default();
            let mut floor: HashMap<PlayerId, u32> = HashMap::new();
            let rand_pos = |next: &mut dyn FnMut() -> u64| loop {
                let f = |x: u64| (x % 1000) as f32 / 1000.0 * 2.0 * ARENA_HALF - ARENA_HALF;
                let p = Vec2::new(f(next()), f(next()));
                if sim_clear(p) {
                    return p;
                }
            };
            for t in 1..=400u32 {
                sim.step_respawns();
                if next() % 10 == 0 {
                    if let Some(p) = sim.players().get(next() as usize % sim.players().len().max(1)).copied() {
                        sim.despawn(p.id);
                    }
                }
                vis.recompute(&sim);
                r.observe(t, &sim, &vis);
                n.observe(t, &sim);
                let shown: HashSet<PlayerId> = sim.players().iter().filter(|p| p.alive).map(|p| p.id).collect();
                // Joins, often onto an id just freed, sometimes the top ones.
                for _ in 0..(next() % 3) {
                    let id = [1, 2, 3, 63, 64, 128, 254, 255, (next() % 40) as u8 + 1][next() as usize % 9];
                    if sim.player(id).is_none() && sim.players().len() < 30 {
                        sim.spawn(id, rand_pos(&mut next));
                        vis.add(&sim, id);
                        r.joined(t, &sim, &vis, id);
                        n.joined(t, &sim, id);
                        floor.insert(id, t);
                    }
                }
                let ids: Vec<PlayerId> = sim.players().iter().map(|p| p.id).collect();
                for &s in &ids {
                    if next() % 3 == 0 {
                        continue; // not firing this tick
                    }
                    // The snapshot it acted on: up to 3 ticks behind, never
                    // older than one it already acted on.
                    let f = floor.entry(s).or_insert(0);
                    let seen = t.saturating_sub((next() % 4) as u32).max(*f);
                    *f = seen;
                    let alive = sim.player(s).is_some_and(|p| p.alive);
                    if alive {
                        r.fired(seen, s);
                        n.fired(seen, s);
                    }
                    let want = naive_target(&sim, &shown, s);
                    let aim = want.and_then(|e| sim.player(e)).map(|e| {
                        let me = sim.player(s).unwrap().pos;
                        Vec2::new(e.pos.x - me.x, e.pos.y - me.y)
                    });
                    let aim = aim.unwrap_or(Vec2::new(1.0, 0.0));
                    let ev = sim.aim_evidence_in(&vis, s, aim, |id| shown.contains(&id));
                    assert_eq!(ev.map(|(_, e)| e), want, "t={t}: evidence target of {s}");
                    let target = match ev {
                        Some((_, e)) if next() % 4 != 0 => Some(e),
                        _ if alive && ids.len() > 1 => Some(ids[next() as usize % ids.len()]).filter(|&e| e != s),
                        _ => None,
                    };
                    if let Some(e) = target {
                        if !sim.player(e).is_some_and(|p| p.alive) {
                            fallen += 1;
                        }
                        let ivs = n.iv.get(&(s, e)).cloned().unwrap_or_default();
                        match n.interval(s, e, seen) {
                            Some((_, false)) => earlier += 1,
                            Some((_, true)) if !n.open((s, e)) => after_left += 1,
                            None if ivs.last().is_some_and(|&(since, _)| seen < since) => unseen += 1,
                            _ => {}
                        }
                        let (got, exp) = (r.engage(s, e, seen), n.engage(s, e, seen));
                        assert_eq!(got, exp, "t={t}: react of {s} at {e}, seen {seen}, intervals {ivs:?}");
                        shots += 1;
                        match got {
                            Some(0) => instant += 1,
                            Some(_) => timed += 1,
                            None => none += 1,
                        }
                    }
                    sim.apply_shot(s, aim);
                }
                for &p in &ids {
                    let a = (next() % 8) as f32 * std::f32::consts::FRAC_PI_4;
                    sim.apply_move(p, Vec2::new(a.cos(), a.sin()));
                }
            }
        }
        assert!(shots > 2_000, "only {shots} shots");
        assert!(instant > 50 && timed > 50 && none > 500, "instant {instant}, timed {timed}, none {none}");
        assert!(fallen > 10, "only {fallen} shots at a target killed earlier in the tick");
        assert!(unseen > 20, "only {unseen} shots at a target newer than the snapshot acted on");
        assert!(after_left > 20, "only {after_left} shots chosen before the target left sight");
        assert!(earlier > 5, "only {earlier} shots in an interval since replaced");
    }

    fn sim_clear(p: Vec2) -> bool {
        Sim::with_walls(&ARENA_WALLS).clear(p, p)
    }
}
