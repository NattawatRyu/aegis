//! Who sees whom this tick, computed once and shared.
//!
//! WHY:   every consumer of line of sight asks the same question about the
//!        same positions: the snapshot (each player's [`Sim::view`]), aim
//!        evidence ([`Sim::aim_error`]) and the reaction detector. Asked
//!        separately, each is its own n² of [`Sim::sees`]; the C6.0 scale
//!        sweep put view-building alone at ~90% of a full room's tick.
//! HOW:   one [`Sim::sees`] per ordered pair, into a bit per (viewer, target).
//!        Not halved by symmetry: `sees` rays the target's hitbox edges, so
//!        `sees(a, b)` and `sees(b, a)` can differ.
//! WHEN:  valid while positions stand still — from after respawns in
//!        `begin_tick` until the moves in `end_tick`. A player spawned in
//!        between is added with [`Visibility::add`]; deaths need nothing
//!        (the dead are culled like the living, and `alive` is read live).
//!
//! Fixed size: 256 rows of 256 bits (8 KB), indexed by [`PlayerId`]; nothing
//! is allocated per tick.

use aegis_protocol::{PlayerId, PlayerState};

use crate::Sim;

const WORDS: usize = 4;
type Row = [u64; WORDS];

pub struct Visibility {
    rows: Box<[Row; 256]>,
}

impl Default for Visibility {
    fn default() -> Self {
        Self { rows: Box::new([[0; WORDS]; 256]) }
    }
}

fn bit(id: PlayerId) -> (usize, u64) {
    (id as usize / 64, 1u64 << (id as usize % 64))
}

impl Visibility {
    /// Line of sight between every pair of players in `sim`, as it stands.
    pub fn compute(sim: &Sim) -> Self {
        let mut v = Self::default();
        v.recompute(sim);
        v
    }

    /// [`Visibility::compute`] into this one, reusing its storage.
    pub fn recompute(&mut self, sim: &Sim) {
        self.rows.iter_mut().for_each(|r| *r = [0; WORDS]);
        let ps = sim.players();
        for a in ps {
            for b in ps {
                if a.id != b.id && sim.sees(a.pos, b.pos) {
                    self.set(a.id, b.id);
                }
            }
        }
    }

    /// Player `id` has just entered `sim` (already spawned): fill in its row
    /// and its column. Any old bits for the id are cleared first.
    pub fn add(&mut self, sim: &Sim, id: PlayerId) {
        self.remove(id);
        let Some(me) = sim.player(id).copied() else { return };
        for p in sim.players().iter().filter(|p| p.id != id) {
            if sim.sees(me.pos, p.pos) {
                self.set(id, p.id);
            }
            if sim.sees(p.pos, me.pos) {
                self.set(p.id, id);
            }
        }
    }

    /// Forget `id`: it sees nobody and nobody sees it.
    pub fn remove(&mut self, id: PlayerId) {
        self.rows[id as usize] = [0; WORDS];
        let (w, m) = bit(id);
        self.rows.iter_mut().for_each(|r| r[w] &= !m);
    }

    fn set(&mut self, viewer: PlayerId, target: PlayerId) {
        let (w, m) = bit(target);
        self.rows[viewer as usize][w] |= m;
    }

    /// Does `viewer` see `target`? Never itself.
    pub fn sees(&self, viewer: PlayerId, target: PlayerId) -> bool {
        let (w, m) = bit(target);
        self.rows[viewer as usize][w] & m != 0
    }

    /// Everyone `viewer` sees, as a bitset indexed by id. For the reaction
    /// detector: `now & !before` is who just came into sight.
    pub fn row(&self, viewer: PlayerId) -> [u64; WORDS] {
        self.rows[viewer as usize]
    }

    /// What `id` is sent: [`Sim::view`] answered from these bits — itself
    /// and everyone it sees, in world order. Empty for an id not in `sim`.
    pub fn view(&self, sim: &Sim, id: PlayerId) -> Vec<PlayerState> {
        if sim.player(id).is_none() {
            return Vec::new();
        }
        sim.players().iter().filter(|p| p.id == id || self.sees(id, p.id)).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::ARENA_WALLS;
    use crate::Wall;
    use aegis_protocol::Vec2;

    fn pillar() -> Sim {
        Sim::with_walls(&[Wall::new(Vec2::new(4.0, -1.0), Vec2::new(6.0, 1.0))])
    }

    /// Bits agree with `Sim::sees` for every ordered pair, both ways.
    fn assert_matches(sim: &Sim, v: &Visibility) {
        for a in sim.players() {
            for b in sim.players() {
                let want = a.id != b.id && sim.sees(a.pos, b.pos);
                assert_eq!(v.sees(a.id, b.id), want, "{} -> {}", a.id, b.id);
            }
            assert_eq!(v.view(sim, a.id), sim.view(a.id), "view of {}", a.id);
        }
    }

    #[test]
    fn matches_sees_and_view_behind_a_wall() {
        let mut sim = pillar();
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0)); // behind the box
        sim.spawn(3, Vec2::new(0.0, 20.0));
        let v = Visibility::compute(&sim);
        assert!(!v.sees(1, 2) && !v.sees(2, 1));
        assert!(v.sees(1, 3) && v.sees(3, 1));
        assert!(!v.sees(1, 1), "a player is not its own target");
        assert_matches(&sim, &v);
        assert!(v.view(&sim, 9).is_empty());
    }

    /// Edge: ids at both ends of the range and on every word boundary.
    #[test]
    fn word_boundaries_and_the_ends_of_the_id_range() {
        let mut sim = Sim::with_walls(&ARENA_WALLS);
        let ids: [PlayerId; 8] = [1, 63, 64, 127, 128, 191, 192, 255];
        for (k, &id) in ids.iter().enumerate() {
            let a = std::f32::consts::TAU * k as f32 / ids.len() as f32;
            sim.spawn(id, Vec2::new(20.0 * a.cos(), 20.0 * a.sin()));
        }
        let v = Visibility::compute(&sim);
        assert_matches(&sim, &v);
        assert!(ids.iter().any(|&a| ids.iter().any(|&b| a != b && !v.sees(a, b))), "pillar hid nobody");
    }

    /// `add` after a mid-tick spawn equals a full recompute; `remove` clears
    /// the row and the column; `recompute` forgets stale bits.
    #[test]
    fn add_remove_and_recompute_agree_with_compute() {
        let mut sim = Sim::with_walls(&ARENA_WALLS);
        for id in 1..=12u8 {
            let a = std::f32::consts::TAU * id as f32 / 12.0;
            sim.spawn(id, Vec2::new(20.0 * a.cos(), 20.0 * a.sin()));
        }
        let mut v = Visibility::compute(&sim);
        sim.spawn(200, Vec2::new(3.0, 30.0));
        v.add(&sim, 200);
        assert_matches(&sim, &v);
        assert_eq!(v.row(200), Visibility::compute(&sim).row(200));

        sim.despawn(5);
        v.remove(5);
        assert!((1..=255).all(|i| !v.sees(5, i) && !v.sees(i, 5)));
        assert_matches(&sim, &v);

        sim.apply_move(1, Vec2::new(0.0, 1.0));
        v.recompute(&sim);
        assert_matches(&sim, &v);
        assert!(v.rows.iter().zip(Visibility::compute(&sim).rows.iter()).all(|(a, b)| a == b));
    }

    #[test]
    fn empty_world() {
        let sim = Sim::new();
        let v = Visibility::compute(&sim);
        assert!(v.rows.iter().all(|r| *r == [0; WORDS]));
    }
}
