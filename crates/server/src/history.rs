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

use crate::sim::{bearing_error, dist2, HIT_RADIUS};
use crate::{Sim, Visibility};

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
        Self { frames: (0..HISTORY).map(|_| empty()).collect() }
    }
}

impl History {
    /// Tick `tick`'s world, as shown: `sim` and its `vis`, read at the start
    /// of the tick before anyone joins or moves. Recording a tick again
    /// replaces it.
    pub fn record(&mut self, tick: u32, sim: &Sim, vis: &Visibility) {
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
        if d2(e) <= HIT_RADIUS * HIT_RADIUS {
            return None;
        }
        bearing_error(aim, me, f.pos[e as usize]).map(|err| (err, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ARENA_HALF, ARENA_WALLS, MAX_HEALTH, SHOT_DAMAGE};
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
        let (mut some, mut gone, mut dead_shooter) = (0, 0, 0);
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
                let mut w = Sim::with_walls(&ARENA_WALLS);
                for p in kept[seen as usize].iter().filter(|p| p.alive) {
                    w.spawn(p.id, p.pos);
                }
                let want = w.aim_evidence(s, aim, |_| true);
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
    }
}
