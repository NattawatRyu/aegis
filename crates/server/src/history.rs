//! The worlds the server has shown, for the last [`HISTORY`] ticks — what a
//! client's shot is judged against.
//!
//! An input says which snapshot it was chosen on (`seen`, proven by
//! `guards::tick_proof`). A client a round trip behind aims at the picture
//! it had, not at the server's world now, so the aim evidence of its shot —
//! which enemy it was at, and how far off — is measured in the frame of
//! `seen`. Measured in the world now (until 2026-10-08), any lag scored
//! shots against enemies the shooter had not been shown yet: honest players
//! read as instant, and an instant bot's every run of fire went untimed.
//!
//! Hits still resolve in the world now: what this changes is the evidence,
//! not the game.
//!
//! Fixed size (~170 KB): [`HISTORY`] frames of 256 positions, 256 alive
//! flags and 256 sight rows, overwritten in place each tick.

use aegis_protocol::{PlayerId, Vec2};

use crate::sim::{bearing_error, dist2, is_usable_aim, HIT_RADIUS};
#[cfg(doc)]
use crate::Sim;
use crate::{Visibility, World};

/// Ticks of shown worlds kept: 16 = 533 ms at 30 Hz. An input claiming an
/// older snapshot is refused (`guards::stale_tick`), so a player with a
/// round trip longer than this cannot play — the price of having every shot
/// judged. Chosen by the user 2026-10-08.
pub const HISTORY: u32 = 16;

const WORDS: usize = 4;
type Row = [u64; WORDS];

fn has(r: &Row, id: PlayerId) -> bool {
    r[id as usize / 64] & (1u64 << (id as usize % 64)) != 0
}

/// One shown world: who was alive (in the snapshots that went out), where,
/// and who each of them could see.
struct Frame {
    /// The tick it was shown on; `None` until first written.
    tick: Option<u32>,
    alive: [bool; 256],
    pos: [Vec2; 256],
    /// `rows[s]`: the living `s` could see, if `s` was alive.
    rows: [Row; 256],
    /// The living in the sim's own order, which breaks ties for "nearest"
    /// exactly as [`Sim::aim_evidence`] does.
    order: Vec<PlayerId>,
}

pub struct History {
    frames: Box<[Frame]>,
    /// An enemy this close is point-blank: any aim might be at it, so it
    /// is no evidence of aim. The lab's is its hitbox ([`HIT_RADIUS`]).
    point_blank: f32,
}

impl Default for History {
    fn default() -> Self {
        let empty = || Frame {
            tick: None,
            alive: [false; 256],
            pos: [Vec2::ZERO; 256],
            rows: [[0; WORDS]; 256],
            order: Vec::with_capacity(256),
        };
        Self { frames: (0..HISTORY).map(|_| empty()).collect(), point_blank: HIT_RADIUS }
    }
}

impl History {
    /// Kept worlds for a game whose hitbox radius (how close is point-blank)
    /// is `point_blank`, in its units.
    pub fn with_point_blank(point_blank: f32) -> Self {
        Self { point_blank, ..Self::default() }
    }

    /// Tick `tick`'s world, as shown: `sim` and its `vis`, read at the start
    /// of the tick before anyone joins or moves. Recording a tick again
    /// replaces it.
    pub fn record(&mut self, tick: u32, sim: &impl World, vis: &Visibility) {
        let f = &mut self.frames[(tick % HISTORY) as usize];
        f.tick = Some(tick);
        f.alive = [false; 256];
        f.order.clear();
        for p in sim.players().iter().filter(|p| p.alive) {
            f.alive[p.id as usize] = true;
            f.pos[p.id as usize] = p.pos;
            f.order.push(p.id);
        }
        let mut alive: Row = [0; WORDS];
        for id in (0..=PlayerId::MAX).filter(|&i| f.alive[i as usize]) {
            alive[id as usize / 64] |= 1u64 << (id as usize % 64);
        }
        for s in 0..=PlayerId::MAX {
            f.rows[s as usize] = if f.alive[s as usize] {
                let r = vis.row(s);
                std::array::from_fn(|w| r[w] & alive[w])
            } else {
                [0; WORDS]
            };
        }
    }

    /// The newest tick recorded, if any.
    pub fn latest(&self) -> Option<u32> {
        self.frames.iter().filter_map(|f| f.tick).max()
    }

    fn frame(&self, tick: u32) -> Option<&Frame> {
        let f = &self.frames[(tick % HISTORY) as usize];
        (f.tick == Some(tick)).then_some(f)
    }

    /// Was `id` alive in the world shown on `tick`? `false` for a tick no
    /// longer (or not yet) kept.
    pub fn alive(&self, tick: u32, id: PlayerId) -> bool {
        self.frame(tick).is_some_and(|f| f.alive[id as usize])
    }

    /// Did `viewer` see `target` alive in the world shown on `tick`? `false`
    /// for a tick no longer (or not yet) kept.
    pub fn sees(&self, tick: u32, viewer: PlayerId, target: PlayerId) -> bool {
        self.frame(tick).is_some_and(|f| has(&f.rows[viewer as usize], target))
    }

    /// The aim evidence of a shot `shooter` chose on snapshot `seen`: the
    /// nearest enemy it was shown there, and the angle between `aim` and the
    /// bearing to it, both from where they stood in that frame. `None` if
    /// the frame is gone, the shooter was not alive in it (it chose to fire
    /// while it was dead on its screen), it saw nobody, the aim is
    /// degenerate, or the enemy was point-blank — see [`Sim::aim_error`].
    pub fn aim_evidence(&self, seen: u32, shooter: PlayerId, aim: Vec2) -> Option<(f32, PlayerId)> {
        let f = self.frame(seen)?;
        if !f.alive[shooter as usize] {
            return None;
        }
        let me = f.pos[shooter as usize];
        let row = &f.rows[shooter as usize];
        let d2 = |e: PlayerId| dist2(me, f.pos[e as usize]);
        let e = f
            .order
            .iter()
            .copied()
            .filter(|&e| e != shooter && has(row, e))
            .min_by(|&a, &b| d2(a).total_cmp(&d2(b)))?;
        if d2(e) <= self.point_blank * self.point_blank {
            return None;
        }
        bearing_error(aim, me, f.pos[e as usize]).map(|err| (err, e))
    }

    /// What the aim of a shot chosen on snapshot `seen` (and resolved on
    /// `now`) says about enemies its picture did not show. `None` — nothing
    /// to judge — unless some enemy the shooter had not been shown on any
    /// kept tick up to `seen` came into its sight between `seen` and `now`.
    ///
    /// A client has seen nothing newer than the snapshot it claims, and
    /// cannot extrapolate an enemy it has never been shown. An aim that fits
    /// such an enemy better than every enemy in the claimed picture was
    /// chosen on a newer snapshot than the one claimed (`StaleLiar`). An
    /// enemy shown before `seen`, hidden there, is remembered, not foreseen:
    /// it counts on neither side.
    ///
    /// `None` too if the claimed frame is gone, the shooter was not alive in
    /// it, or the aim is degenerate.
    pub fn glimpse(&self, seen: u32, now: u32, shooter: PlayerId, aim: Vec2) -> Option<Glimpse> {
        let f = self.frame(seen)?;
        if !f.alive[shooter as usize] || !is_usable_aim(aim) {
            return None;
        }
        let me = f.pos[shooter as usize];
        let mut claimed = f32::INFINITY;
        for e in ids(&f.rows[shooter as usize]).filter(|&e| e != shooter) {
            let to = f.pos[e as usize];
            // Point-blank, any aim might be at it: it explains everything.
            let err = if dist2(me, to) <= self.point_blank * self.point_blank {
                Some(0.0)
            } else {
                bearing_error(aim, me, to)
            };
            claimed = claimed.min(err.unwrap_or(f32::INFINITY));
        }
        // Everyone shown to it on a kept tick up to `seen`, and itself.
        let mut known: Row = [0; WORDS];
        known[shooter as usize / 64] |= 1u64 << (shooter as usize % 64);
        for t in seen.saturating_sub(HISTORY - 1)..=seen {
            if let Some(g) = self.frame(t) {
                for (k, r) in known.iter_mut().zip(g.rows[shooter as usize]) {
                    *k |= r;
                }
            }
        }
        // Each enemy new to it, where it was on the first tick it was shown.
        let mut ahead = f32::INFINITY;
        for t in seen + 1..=now {
            let Some(g) = self.frame(t) else { continue };
            let row = g.rows[shooter as usize];
            let new: Row = std::array::from_fn(|w| row[w] & !known[w]);
            for (k, r) in known.iter_mut().zip(row) {
                *k |= r;
            }
            let from = g.pos[shooter as usize];
            for e in ids(&new) {
                let to = g.pos[e as usize];
                if dist2(from, to) > self.point_blank * self.point_blank {
                    ahead = ahead.min(bearing_error(aim, from, to).unwrap_or(f32::INFINITY));
                }
            }
        }
        ahead.is_finite().then_some(Glimpse { claimed, ahead })
    }
}

/// The ids in a sight row, ascending.
fn ids(r: &Row) -> impl Iterator<Item = PlayerId> + '_ {
    (0..=PlayerId::MAX).filter(move |&id| has(r, id))
}

/// A shot's aim against two pictures: the one its input claimed, and the
/// enemies that only newer snapshots showed. See [`History::glimpse`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glimpse {
    /// Smallest bearing error to an enemy in the claimed picture, from where
    /// they stood there; 0 if one was point-blank, infinite if none.
    pub claimed: f32,
    /// Smallest bearing error to an enemy first shown after the claimed
    /// tick, measured on the tick it was first shown (point-blank ones are
    /// not counted).
    pub ahead: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ARENA_HALF, ARENA_WALLS, MAX_HEALTH, SHOT_DAMAGE};
    use crate::Sim;
    use aegis_protocol::PlayerState;

    /// The oracle: a random walled world, moving, killing and respawning,
    /// with players leaving and joining. Every tick's world is kept as a
    /// plain copy. For random shooters, aims and claimed ticks up to
    /// HISTORY + 4 back, the frame's evidence must equal — to the bit — the
    /// sim's own ray-cast evidence in a world rebuilt from that copy; and a
    /// tick HISTORY or more back (or ahead) must give `None`.
    #[test]
    fn matches_a_world_rebuilt_from_each_kept_tick() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let coord = |x: u64| (x % 1000) as f32 / 1000.0 * 2.0 * ARENA_HALF - ARENA_HALF;
        let mut sim = Sim::with_walls(&ARENA_WALLS);
        for id in 1..=20 {
            sim.spawn(id, Vec2::new(coord(next()), coord(next())));
        }
        let (mut h, mut vis) = (History::default(), Visibility::default());
        let mut kept: Vec<Vec<PlayerState>> = vec![Vec::new()]; // index = tick
        let (mut some, mut gone, mut dead_shooter, mut open, mut near) = (0, 0, 0, 0, 0);
        for t in 1..=300u32 {
            sim.step_respawns();
            if next() % 15 == 0 {
                let id = (next() % 20) as PlayerId + 1;
                match sim.player(id) {
                    Some(_) => sim.despawn(id),
                    None => sim.spawn(id, Vec2::new(coord(next()), coord(next()))),
                }
            }
            vis.recompute(&sim);
            h.record(t, &sim, &vis);
            kept.push(sim.players().to_vec());
            for _ in 0..20 {
                let s = (next() % 20) as PlayerId + 1;
                let back = (next() % (HISTORY as u64 + 5)) as u32;
                let Some(seen) = t.checked_sub(back) else { continue };
                let a = (next() % 628) as f32 / 100.0;
                let aim = Vec2::new(a.cos(), a.sin());
                let got = h.aim_evidence(seen, s, aim);
                if back >= HISTORY || seen == 0 {
                    assert_eq!(got, None, "t={t}: tick {seen} should be gone");
                    gone += 1;
                    continue;
                }
                let w = world(&kept, seen);
                let want = w.aim_evidence(s, aim, |_| true);
                let g = h.glimpse(seen, t, s, aim);
                let naive = naive_glimpse(&kept, seen, t, s, aim);
                assert_eq!(g.is_some(), naive.is_some(), "t={t}: glimpse of {s} on {seen}: {g:?} vs {naive:?}");
                if let (Some(g), Some((c, a))) = (g, naive) {
                    let close = |x: f32, y: f32| x == y || (x - y).abs() < 1e-4;
                    assert!(close(g.claimed, c) && close(g.ahead, a), "t={t}: {s} on {seen}: {g:?} vs {c} {a}");
                    open += 1;
                    near += u32::from(g.ahead < 0.3);
                }
                assert_eq!(
                    got.map(|(e, id)| (e.to_bits(), id)),
                    want.map(|(e, id)| (e.to_bits(), id)),
                    "t={t}: shooter {s} on tick {seen}"
                );
                some += u32::from(got.is_some());
                dead_shooter += u32::from(kept[seen as usize].iter().any(|p| p.id == s && !p.alive));
                assert_eq!(h.alive(seen, s), kept[seen as usize].iter().any(|p| p.id == s && p.alive));
            }
            assert!(!h.alive(t + 1, 1), "a tick not yet recorded");
            // Moves and kills, for the next tick's world.
            for p in sim.players().iter().map(|p| p.id).collect::<Vec<_>>() {
                let a = (next() % 8) as f32 * std::f32::consts::FRAC_PI_4;
                sim.apply_move(p, Vec2::new(a.cos(), a.sin()));
                if next() % 25 == 0 {
                    let a = (next() % 628) as f32 / 100.0;
                    for _ in 0..(MAX_HEALTH / SHOT_DAMAGE) {
                        sim.apply_shot(p, Vec2::new(a.cos(), a.sin()));
                    }
                }
            }
        }
        assert!(some > 1_000 && gone > 300 && dead_shooter > 50, "some {some}, gone {gone}, dead {dead_shooter}");
        assert!(open > 200 && near > 20, "open {open}, near {near}");
    }

    /// The world of tick `t` rebuilt from its plain copy.
    fn world(kept: &[Vec<PlayerState>], t: u32) -> Sim {
        let mut w = Sim::with_walls(&ARENA_WALLS);
        for p in kept[t as usize].iter().filter(|p| p.alive) {
            w.spawn(p.id, p.pos);
        }
        w
    }

    /// [`History::glimpse`] the slow way, from rebuilt worlds, the sim's own
    /// view and plain angles: (claimed, ahead).
    fn naive_glimpse(kept: &[Vec<PlayerState>], seen: u32, now: u32, s: PlayerId, aim: Vec2) -> Option<(f32, f32)> {
        use std::collections::BTreeSet;
        use std::f32::consts::TAU;
        let angle = |from: Vec2, to: Vec2| {
            let d = ((to.y - from.y).atan2(to.x - from.x) - aim.y.atan2(aim.x)).rem_euclid(TAU);
            d.min(TAU - d)
        };
        let blank = |a: Vec2, b: Vec2| (b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y) <= HIT_RADIUS * HIT_RADIUS;
        let w = world(kept, seen);
        let me = w.player(s)?.pos;
        let mut claimed = f32::INFINITY;
        for e in w.view(s).into_iter().filter(|p| p.id != s) {
            claimed = claimed.min(if blank(me, e.pos) { 0.0 } else { angle(me, e.pos) });
        }
        let mut known = BTreeSet::from([s]);
        for t in (now + 1).saturating_sub(HISTORY).max(1)..=seen {
            known.extend(world(kept, t).view(s).iter().map(|p| p.id));
        }
        let mut ahead = f32::INFINITY;
        for t in seen + 1..=now {
            let w = world(kept, t);
            let Some(from) = w.player(s).map(|p| p.pos) else { continue };
            for e in w.view(s) {
                if known.insert(e.id) && !blank(from, e.pos) {
                    ahead = ahead.min(angle(from, e.pos));
                }
            }
        }
        ahead.is_finite().then_some((claimed, ahead))
    }
}
