//! Everything the server measures about a shot, for a game that is not the
//! Aegis sim: line of sight, the shown worlds, reaction times — over any
//! [`World`].
//!
//! The Aegis [`crate::Server`] runs on this too, so an engine gets exactly
//! the evidence the lab's detector thresholds were measured on. Per tick:
//!
//! ```text
//! begin_tick(tick, world)     positions settled, snapshots about to go out
//! joined(tick, world, id)     a player admitted mid-tick (already in world)
//! shot(tick, seen, ...)       each shot, in the order the game resolves them
//! ```
//!
//! and each [`ShotEvidence`] becomes telemetry: `Outcome::Shot` when it has
//! `aim`, `Outcome::Glimpse` when it has `glimpse` — which the detector
//! (`aegis_detector::Monitor`) reads.
//!
//! `seen` is the tick of the snapshot the shooter's input was chosen on. The
//! game must prove it (the Aegis protocol echoes a MAC'd proof, guarded by
//! `guards::tick_proof`) and keep it within the last
//! [`HISTORY`](crate::history::HISTORY) ticks and never going back
//! (`guards::stale_tick`): an unproven `seen` lets a client pick the picture
//! it is judged in.

use aegis_protocol::{PlayerId, Vec2};

use crate::history::{Glimpse, History};
use crate::reaction::Reaction;
use crate::sim::HIT_RADIUS;
use crate::{Visibility, World};

/// What one shot says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShotEvidence {
    /// The shooter was alive now and in the picture it fired on. A shot
    /// that is not live is no evidence and starts no run of fire.
    pub live: bool,
    /// Aim error (radians) to the nearest enemy in the shooter's picture,
    /// that enemy, and the reaction time if this shot is timed. `None`: no
    /// enemy in the picture, one point-blank, or a degenerate aim.
    pub aim: Option<(f32, PlayerId, Option<u32>)>,
    /// The aim against enemies only a newer snapshot showed.
    pub glimpse: Option<Glimpse>,
}

pub struct Evidence {
    vis: Visibility,
    history: History,
    react: Reaction,
}

impl Default for Evidence {
    /// At the lab's hitbox radius.
    fn default() -> Self {
        Self::with_point_blank(HIT_RADIUS)
    }
}

impl Evidence {
    /// For a game whose hitbox radius is `point_blank`, in its units: an
    /// enemy that close is no evidence of aim.
    pub fn with_point_blank(point_blank: f32) -> Self {
        Self { vis: Visibility::default(), history: History::with_point_blank(point_blank), react: Reaction::default() }
    }

    /// Start of tick `tick`: positions are where this tick's snapshots show
    /// them. Computes line of sight, keeps the shown world, starts and ends
    /// sight intervals.
    pub fn begin_tick(&mut self, tick: u32, world: &impl World) {
        self.vis.recompute(world);
        self.history.record(tick, world, &self.vis);
        self.react.observe(tick, world, &self.vis);
    }

    /// Player `id` was admitted mid-tick and is already in `world`: a new
    /// person, whatever the id held before. In no snapshot yet, so nobody's
    /// evidence until the next tick.
    pub fn joined(&mut self, tick: u32, world: &impl World, id: PlayerId) {
        self.vis.add(world, id);
        self.react.joined(tick, world, &self.vis, id);
    }

    /// A shot by `shooter` at `aim`, chosen on snapshot `seen`, resolved on
    /// `tick`. `alive`: the shooter is alive now, in the game's world. Call
    /// once per shot, in resolution order, before the game resolves it or
    /// after — the evidence is measured in the shown worlds, not the world
    /// the shot hits in.
    pub fn shot(&mut self, tick: u32, seen: u32, shooter: PlayerId, alive: bool, aim: Vec2) -> ShotEvidence {
        let live = alive && self.history.alive(seen, shooter);
        if !live {
            return ShotEvidence { live, aim: None, glimpse: None };
        }
        self.react.fired(seen, shooter);
        let glimpse = self.history.glimpse(seen, tick, shooter, aim);
        let aim = self
            .history
            .aim_evidence(seen, shooter, aim)
            .map(|(err, enemy)| (err, enemy, self.react.engage(shooter, enemy, seen)));
        ShotEvidence { live, aim, glimpse }
    }

    /// This tick's line of sight.
    pub fn visibility(&self) -> &Visibility {
        &self.vis
    }

    /// The shown worlds kept.
    pub fn history(&self) -> &History {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    /// An engine's world that is not the sim: open ground, sight ends in fog
    /// at 30 units.
    struct Fog(Vec<PlayerState>);

    impl World for Fog {
        fn players(&self) -> &[PlayerState] {
            &self.0
        }

        fn sees(&self, from: Vec2, to: Vec2) -> bool {
            Vec2::new(to.x - from.x, to.y - from.y).len() < 30.0
        }
    }

    fn at(id: PlayerId, x: f32) -> PlayerState {
        PlayerState { id, pos: Vec2::new(x, 0.0), health: 100, alive: true }
    }

    const EAST: Vec2 = Vec2 { x: 1.0, y: 0.0 };

    /// Enemy 2 walks out of the fog on tick 2. A shot on that very snapshot
    /// is timed 0; the next in the engagement is not timed; a shot claiming
    /// tick 1 (fog only) that fits 2 is a glimpse with nothing claimed.
    #[test]
    fn an_engine_world_gets_reaction_aim_and_glimpse() {
        let mut ev = Evidence::default();
        let mut w = Fog(vec![at(1, 0.0), at(2, 40.0)]);
        ev.begin_tick(1, &w);
        w.0[1] = at(2, 20.0);
        ev.begin_tick(2, &w);
        let s = ev.shot(2, 2, 1, true, EAST);
        assert_eq!(
            (s.live, s.aim.map(|(e, id, r)| (e < 1e-6, id, r)), s.glimpse),
            (true, Some((true, 2, Some(0))), None)
        );
        let s = ev.shot(2, 2, 1, true, EAST);
        assert_eq!(s.aim.map(|(_, _, r)| r), Some(None), "the second shot of an engagement is not timed");

        let mut ev = Evidence::default();
        let mut w = Fog(vec![at(1, 0.0), at(2, 40.0)]);
        ev.begin_tick(1, &w);
        w.0[1] = at(2, 20.0);
        ev.begin_tick(2, &w);
        ev.begin_tick(3, &w);
        let s = ev.shot(3, 1, 1, true, EAST);
        assert_eq!(s.aim, None, "nobody in the claimed picture");
        let g = s.glimpse.expect("a glimpse");
        assert!(g.claimed.is_infinite() && g.ahead < 1e-6, "{g:?}");
    }

    /// The game's hitbox decides point-blank: at 20 units an enemy is
    /// evidence under the lab's radius and none under a 25-unit one.
    #[test]
    fn point_blank_is_the_games_radius() {
        let w = Fog(vec![at(1, 0.0), at(2, 20.0)]);
        for (mut ev, some) in [(Evidence::default(), true), (Evidence::with_point_blank(25.0), false)] {
            ev.begin_tick(1, &w);
            assert_eq!(ev.shot(1, 1, 1, true, EAST).aim.is_some(), some);
        }
    }

    /// Dead now, or dead in the claimed picture: no evidence, no run of fire.
    #[test]
    fn a_dead_trigger_is_no_evidence() {
        let mut w = Fog(vec![at(1, 0.0), at(2, 20.0)]);
        w.0[0].alive = false;
        let mut ev = Evidence::default();
        ev.begin_tick(1, &w);
        w.0[0].alive = true;
        ev.begin_tick(2, &w);
        assert!(!ev.shot(2, 1, 1, true, EAST).live, "dead in the picture it fired on");
        assert!(!ev.shot(2, 2, 1, false, EAST).live, "dead now");
        let s = ev.shot(2, 2, 1, true, EAST);
        assert_eq!(s.aim.map(|(_, _, r)| r), Some(Some(0)), "the dead shots started no run: this one is timed");
    }
}
