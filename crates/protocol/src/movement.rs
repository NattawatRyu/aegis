//! How a step moves a player — the one piece of the simulation a client runs
//! too (pillar B, B3). The server folds each input with [`step`]; a client
//! predicting its own position replays its unacknowledged inputs with the
//! same function, so with nothing lost its prediction is the server's
//! answer to the bit. Two copies of this code would drift; one cannot.
//!
//! Only movement is shared. Shots, health and respawns stay on the server:
//! a client predicts where it is, never whether it hit.

use crate::Vec2;

/// Distance a player travels per tick along a unit direction.
pub const MOVE_SPEED: f32 = 5.0;
/// Half-width of the square arena; positions are clamped to it.
pub const ARENA_HALF: f32 = 50.0;

/// An axis-aligned box nothing can see, shoot or walk through. Its edges
/// count as solid: a line that grazes a corner is blocked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    pub min: Vec2,
    pub max: Vec2,
}

impl Wall {
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// Does the segment `a -> b` touch this box? Slab test: clip the segment's
    /// parameter range to each axis's slab; it hits iff something is left.
    pub fn blocks(&self, a: Vec2, b: Vec2) -> bool {
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for (a, d, lo, hi) in [(a.x, b.x - a.x, self.min.x, self.max.x), (a.y, b.y - a.y, self.min.y, self.max.y)] {
            if d == 0.0 {
                if a < lo || a > hi {
                    return false;
                }
                continue;
            }
            let (u, v) = ((lo - a) / d, (hi - a) / d);
            t0 = t0.max(u.min(v));
            t1 = t1.min(u.max(v));
            if t0 > t1 {
                return false;
            }
        }
        true
    }
}

/// The lab arena: a pillar in the middle, so players across the spawn ring
/// cannot see each other, and one box in each corner. None touches the spawn
/// ring (radius 20). Any wall that hides a player straight across the ring
/// sits on a diameter, so some walk runs into the pillar — the honest bot
/// turns when a step goes nowhere.
pub const ARENA_WALLS: [Wall; 5] = [
    Wall::new(Vec2 { x: -5.0, y: -5.0 }, Vec2 { x: 5.0, y: 5.0 }),
    Wall::new(Vec2 { x: 26.0, y: 26.0 }, Vec2 { x: 34.0, y: 34.0 }),
    Wall::new(Vec2 { x: -34.0, y: 26.0 }, Vec2 { x: -26.0, y: 34.0 }),
    Wall::new(Vec2 { x: -34.0, y: -34.0 }, Vec2 { x: -26.0, y: -26.0 }),
    Wall::new(Vec2 { x: 26.0, y: -34.0 }, Vec2 { x: 34.0, y: -26.0 }),
];

/// Is the straight line `a -> b` clear of every wall?
pub fn clear(walls: &[Wall], a: Vec2, b: Vec2) -> bool {
    !walls.iter().any(|w| w.blocks(a, b))
}

/// Where a living player at `from` ends up after one step along `dir_unit`
/// (already clamped to unit length — the server's move_speed guard does it,
/// a predicting client calls [`Vec2::clamped_unit`] the same way). Clamped
/// to the arena; a step whose path touches a wall is not taken at all — the
/// whole path, not just where it ends, so it never tunnels a thin wall.
pub fn step(walls: &[Wall], from: Vec2, dir_unit: Vec2) -> Vec2 {
    let nx = (from.x + dir_unit.x * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
    let ny = (from.y + dir_unit.y * MOVE_SPEED).clamp(-ARENA_HALF, ARENA_HALF);
    let to = Vec2::new(nx, ny);
    if clear(walls, from, to) {
        to
    } else {
        from
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_clamped_to_the_arena() {
        assert_eq!(step(&[], Vec2::new(ARENA_HALF - 1.0, 0.0), Vec2::new(1.0, 0.0)), Vec2::new(ARENA_HALF, 0.0));
    }

    /// The whole path is checked, so a thin wall cannot be stepped through.
    #[test]
    fn a_step_into_a_wall_is_not_taken() {
        let thin = [Wall::new(Vec2::new(2.0, -1.0), Vec2::new(2.1, 1.0))];
        assert_eq!(step(&thin, Vec2::ZERO, Vec2::new(1.0, 0.0)), Vec2::ZERO);
        assert_eq!(step(&thin, Vec2::ZERO, Vec2::new(-1.0, 0.0)), Vec2::new(-MOVE_SPEED, 0.0));
    }
}
