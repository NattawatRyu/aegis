//! What the evidence needs to know about a game's world, and nothing more.
//!
//! Line of sight ([`crate::Visibility`]), the shown worlds a shot is judged
//! in ([`crate::history`]) and reaction times ([`crate::reaction`]) only ever
//! ask two things of the world: who is where (and alive), and whether one
//! point can see another. An engine with its own simulation answers those
//! two and gets every piece of evidence the Aegis server writes, without
//! [`Sim`] — through [`crate::Evidence`].
//!
//! Aegis's geometry is 2D: positions are on the ground plane, and an aim is
//! a direction on it. A 3D engine projects; a game whose fights depend on
//! height needs more than this trait gives.

use aegis_protocol::{PlayerId, PlayerState, Vec2};

use crate::Sim;

pub trait World {
    /// Every player, alive or dead, in an order that stays the same from
    /// tick to tick: ties for "the nearest enemy" break by it, and the
    /// evidence must not depend on a hash map's mood. `health` is not read.
    fn players(&self) -> &[PlayerState];

    /// Can a player standing at `from` see a player standing at `to`? Need
    /// not be symmetric (Aegis rays the target's hitbox edges).
    fn sees(&self, from: Vec2, to: Vec2) -> bool;

    fn player(&self, id: PlayerId) -> Option<&PlayerState> {
        self.players().iter().find(|p| p.id == id)
    }
}

impl World for Sim {
    fn players(&self) -> &[PlayerState] {
        Sim::players(self)
    }

    fn sees(&self, from: Vec2, to: Vec2) -> bool {
        Sim::sees(self, from, to)
    }

    fn player(&self, id: PlayerId) -> Option<&PlayerState> {
        Sim::player(self, id)
    }
}
