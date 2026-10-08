//! How long a shooter took to fire at an enemy that came into sight — the
//! evidence a reaction-time detector reads (`Outcome::Shot::react`).
//!
//! An *engagement* is shooter S and enemy E, from the tick E enters S's
//! sight until E leaves it. Its first evidence shot carries
//! `react = tick - since`: 0 means S fired on the tick the snapshot first
//! showing E went out. Later shots in the same engagement carry `None`.
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
//! the client sends can set it. Fixed size per room (~270 KB): nothing is
//! allocated per tick.

use aegis_protocol::PlayerId;

use crate::{Sim, Visibility};

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
    /// `since[s][e]`: the tick `e` last came into `s`'s sight. Read only
    /// while `seen[s]` has `e`.
    since: Box<[[u32; 256]; 256]>,
    /// Enemies `s` has already put on record in their current engagement.
    engaged: Box<[Row; 256]>,
    /// Last tick `s` fired, and the first tick of its unbroken run of firing.
    last_shot: [Option<u32>; 256],
    streak_start: [u32; 256],
}

impl Default for Reaction {
    fn default() -> Self {
        Self {
            seen: Box::new([[0; WORDS]; 256]),
            since: Box::new([[0; 256]; 256]),
            engaged: Box::new([[0; WORDS]; 256]),
            last_shot: [None; 256],
            streak_start: [0; 256],
        }
    }
}

/// Bitset of the living.
fn alive(sim: &Sim) -> Row {
    let mut r = [0; WORDS];
    for p in sim.players().iter().filter(|p| p.alive) {
        let (w, m) = bit(p.id);
        r[w] |= m;
    }
    r
}

impl Reaction {
    /// Start of tick `tick`, after `vis` was computed on `sim`: every pair
    /// newly in sight starts an engagement now.
    pub fn observe(&mut self, tick: u32, sim: &Sim, vis: &Visibility) {
        let alive = alive(sim);
        for s in 0..=PlayerId::MAX {
            let now = if has(&alive, s) {
                let row = vis.row(s);
                std::array::from_fn(|w| row[w] & alive[w])
            } else {
                [0; WORDS]
            };
            let before = self.seen[s as usize];
            for w in 0..WORDS {
                let mut new = now[w] & !before[w];
                self.engaged[s as usize][w] &= now[w] & !new;
                while new != 0 {
                    let e = w * 64 + new.trailing_zeros() as usize;
                    self.since[s as usize][e] = tick;
                    new &= new - 1;
                }
            }
            self.seen[s as usize] = now;
        }
    }

    /// Player `id` was admitted mid-tick (already in `sim` and `vis`): a new
    /// person, whatever the id held before. This tick's snapshots have gone
    /// out without it, so everything in sight between it and anyone else
    /// starts with the next one, `tick + 1`.
    pub fn joined(&mut self, tick: u32, sim: &Sim, vis: &Visibility, id: PlayerId) {
        let tick = tick.wrapping_add(1);
        let alive = alive(sim);
        let (w, m) = bit(id);
        self.last_shot[id as usize] = None;
        self.seen[id as usize] = [0; WORDS];
        self.engaged[id as usize] = [0; WORDS];
        for s in 0..=PlayerId::MAX {
            self.seen[s as usize][w] &= !m;
            self.engaged[s as usize][w] &= !m;
        }
        if !has(&alive, id) {
            return;
        }
        for e in sim.players().iter().filter(|p| p.alive && p.id != id).map(|p| p.id) {
            if vis.sees(id, e) {
                let (ew, em) = bit(e);
                self.seen[id as usize][ew] |= em;
                self.since[id as usize][e as usize] = tick;
            }
            if vis.sees(e, id) {
                self.seen[e as usize][w] |= m;
                self.since[e as usize][id as usize] = tick;
            }
        }
    }

    /// `s` fired on `tick` (any shot, evidence or not): extends or starts its
    /// streak. Call before [`Reaction::engage`] for the same shot.
    pub fn fired(&mut self, tick: u32, s: PlayerId) {
        let i = s as usize;
        if !matches!(self.last_shot[i], Some(t) if t == tick || t.wrapping_add(1) == tick) {
            self.streak_start[i] = tick;
        }
        self.last_shot[i] = Some(tick);
    }

    /// `s` put a shot on record against `e` on `tick`. Its reaction time if
    /// this is the first in the engagement and the first of its run of
    /// firing (not prefire, not a target switch), else `None`.
    /// `None` too if `e` is not in `s`'s sight as tracked here (it should
    /// always be: the evidence target is a seen enemy), or if no snapshot has
    /// shown `e` to `s` yet (it joined this tick): a blind shot times nothing.
    pub fn engage(&mut self, tick: u32, s: PlayerId, e: PlayerId) -> Option<u32> {
        let (si, (w, m)) = (s as usize, bit(e));
        if self.seen[si][w] & m == 0 || self.engaged[si][w] & m != 0 {
            return None;
        }
        let since = self.since[si][e as usize];
        let react = tick.checked_sub(since)?;
        self.engaged[si][w] |= m;
        (self.streak_start[si] == tick).then_some(react)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ARENA_HALF, ARENA_WALLS, HIT_RADIUS, MAX_HEALTH, SHOT_DAMAGE};
    use crate::Wall;
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
        r.engage(t, s, e)
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
                        out = r.engage(t, 1, 2);
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

    /// The same rules, written the slow obvious way: sight from `Sim::sees`
    /// every time, maps instead of bits.
    #[derive(Default)]
    struct Naive {
        since: HashMap<(PlayerId, PlayerId), u32>,
        engaged: HashSet<(PlayerId, PlayerId)>,
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
        fn observe(&mut self, t: u32, sim: &Sim) {
            let now = in_sight(sim);
            self.since.retain(|k, _| now.contains(k));
            self.engaged.retain(|k| now.contains(k));
            for k in now {
                self.since.entry(k).or_insert(t);
            }
        }

        fn joined(&mut self, t: u32, sim: &Sim, id: PlayerId) {
            self.since.retain(|&(a, b), _| a != id && b != id);
            self.engaged.retain(|&(a, b)| a != id && b != id);
            self.last.remove(&id);
            for k in in_sight(sim).into_iter().filter(|&(a, b)| a == id || b == id) {
                self.since.insert(k, t + 1);
            }
        }

        fn fired(&mut self, t: u32, s: PlayerId) {
            if !matches!(self.last.get(&s), Some(&l) if l == t || l + 1 == t) {
                self.start.insert(s, t);
            }
            self.last.insert(s, t);
        }

        fn engage(&mut self, t: u32, s: PlayerId, e: PlayerId) -> Option<u32> {
            let &since = self.since.get(&(s, e))?;
            if t < since || !self.engaged.insert((s, e)) {
                return None;
            }
            (self.start[&s] == t).then(|| t - since)
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
    /// leaves, kills and respawns; every evidence shot's reaction must match
    /// the naive version exactly. Also checks the run exercised every branch.
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
        for _world in 0..4 {
            let mut sim = Sim::with_walls(&ARENA_WALLS);
            let (mut r, mut n) = (Reaction::default(), Naive::default());
            let mut vis = Visibility::default();
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
                    }
                }
                let ids: Vec<PlayerId> = sim.players().iter().map(|p| p.id).collect();
                for &s in &ids {
                    if next() % 3 == 0 {
                        continue; // not firing this tick
                    }
                    let alive = sim.player(s).is_some_and(|p| p.alive);
                    if alive {
                        r.fired(t, s);
                        n.fired(t, s);
                    }
                    let want = naive_target(&sim, &shown, s);
                    let aim = want.and_then(|e| sim.player(e)).map(|e| {
                        let me = sim.player(s).unwrap().pos;
                        Vec2::new(e.pos.x - me.x, e.pos.y - me.y)
                    });
                    let aim = aim.unwrap_or(Vec2::new(1.0, 0.0));
                    let ev = sim.aim_evidence_in(&vis, s, aim, |id| shown.contains(&id));
                    assert_eq!(ev.map(|(_, e)| e), want, "t={t}: evidence target of {s}");
                    if let Some((_, e)) = ev {
                        if !sim.player(e).is_some_and(|p| p.alive) {
                            fallen += 1; // killed earlier this tick
                        }
                        let (got, exp) = (r.engage(t, s, e), n.engage(t, s, e));
                        assert_eq!(got, exp, "t={t}: react of {s} at {e}");
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
        assert!(shots > 2_000, "only {shots} evidence shots");
        assert!(instant > 50 && timed > 50 && none > 500, "instant {instant}, timed {timed}, none {none}");
        assert!(fallen > 10, "only {fallen} shots at a target killed earlier in the tick");
    }

    fn sim_clear(p: Vec2) -> bool {
        Sim::with_walls(&ARENA_WALLS).clear(p, p)
    }
}
