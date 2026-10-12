//! Client-side prediction of the player's own position (pillar B, B3).
//!
//! A snapshot is a round trip old by the time it shows the player its own
//! input. A predicting client does not wait: it starts from where the
//! newest snapshot put it — after the input that snapshot acknowledges
//! (`ack`) — and replays every input it sent since with
//! [`aegis_protocol::movement::step`], the code the server folds them with.
//! With nothing lost or refused, the prediction is the server's answer to
//! the bit; this keeps score of that ([`PredictStats`]).
//!
//! It predicts only movement. Whether a shot hit, health and respawns are
//! the server's alone — and a snapshot that shows the player dead, or a
//! death the client did not foresee, is simply the new starting point.

use std::collections::VecDeque;

use aegis_protocol::movement::{step, Wall};
use aegis_protocol::{PlayerState, Vec2};

/// Inputs kept waiting for an ack. Past it the oldest is dropped: a server
/// that acknowledges nothing for this long is not being predicted, it is
/// gone.
pub const MAX_PENDING: usize = 64;

/// How a prediction compared with the server, checked once per acked input
/// that was predicted from a snapshot showing the player alive and was
/// acked by one still showing it alive.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PredictStats {
    pub checked: u32,
    /// Of those, predicted to the bit.
    pub exact: u32,
    /// Largest distance between prediction and server.
    pub max_err: f32,
    pub sum_err: f64,
}

impl PredictStats {
    pub fn mean_err(&self) -> f64 {
        self.sum_err / f64::from(self.checked.max(1))
    }

    /// Pool another player's score into this one.
    pub fn add(&mut self, o: &PredictStats) {
        self.checked += o.checked;
        self.exact += o.exact;
        self.max_err = self.max_err.max(o.max_err);
        self.sum_err += o.sum_err;
    }
}

pub struct Predictor {
    walls: Vec<Wall>,
    /// (tick, ack, this player) from the newest snapshot that showed it.
    base: Option<(u32, u32, PlayerState)>,
    /// Inputs sent after `base`'s ack, oldest first: seq and the step the
    /// server will take for it (`None`: one it refuses outright).
    pending: VecDeque<(u32, Option<Vec2>)>,
    /// Where each input predicted from a living base should leave the player.
    expect: VecDeque<(u32, Vec2)>,
    stats: PredictStats,
}

/// The step the server takes for an input: none for a non-finite one (the
/// sanity guard refuses it), else `move_dir` clamped to unit length, as the
/// move_speed guard does.
fn server_step(move_dir: Vec2, aim: Vec2) -> Option<Vec2> {
    let finite = |v: Vec2| v.x.is_finite() && v.y.is_finite();
    (finite(move_dir) && finite(aim)).then(|| move_dir.clamped_unit())
}

impl Predictor {
    /// A predictor for an arena with `walls` — the server's.
    pub fn new(walls: &[Wall]) -> Self {
        Self {
            walls: walls.to_vec(),
            base: None,
            pending: VecDeque::new(),
            expect: VecDeque::new(),
            stats: PredictStats::default(),
        }
    }

    /// The snapshot of `tick` acknowledged input `ack` and showed this player
    /// as `me` (`None`: not in it). An older one than the newest seen is
    /// ignored — it was overtaken on the way.
    pub fn snapshot(&mut self, tick: u32, ack: u32, me: Option<PlayerState>) {
        if self.base.is_some_and(|(t, a, _)| tick <= t || ack < a) {
            return;
        }
        let Some(me) = me else { return };
        if me.alive {
            if let Some(&(_, at)) = self.expect.iter().find(|&&(s, _)| s == ack) {
                let err = Vec2::new(at.x - me.pos.x, at.y - me.pos.y).len();
                self.stats.checked += 1;
                self.stats.exact += u32::from(at == me.pos);
                self.stats.max_err = self.stats.max_err.max(err);
                self.stats.sum_err += f64::from(err);
            }
        }
        self.expect.retain(|&(s, _)| s > ack);
        self.pending.retain(|&(s, _)| s > ack);
        self.base = Some((tick, ack, me));
    }

    /// Input `seq` was sent. Returns where it should leave the player, if
    /// a living base is known.
    pub fn input(&mut self, seq: u32, move_dir: Vec2, aim: Vec2) -> Option<Vec2> {
        self.pending.push_back((seq, server_step(move_dir, aim)));
        if self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
        let at = self.position().filter(|_| self.base.is_some_and(|(_, _, me)| me.alive))?;
        self.expect.push_back((seq, at));
        if self.expect.len() > MAX_PENDING {
            self.expect.pop_front();
        }
        Some(at)
    }

    /// Where the player is once every input sent has been folded: the
    /// newest snapshot's position, stepped by each unacknowledged input. A
    /// dead player stays where it fell.
    pub fn position(&self) -> Option<Vec2> {
        let (_, _, me) = self.base?;
        if !me.alive {
            return Some(me.pos);
        }
        Some(self.pending.iter().fold(me.pos, |p, &(_, d)| d.map_or(p, |d| step(&self.walls, p, d))))
    }

    pub fn stats(&self) -> PredictStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::movement::{ARENA_WALLS, MOVE_SPEED};

    fn me(x: f32, alive: bool) -> Option<PlayerState> {
        Some(PlayerState { id: 1, pos: Vec2::new(x, 20.0), health: 100, alive })
    }

    const EAST: Vec2 = Vec2 { x: 1.0, y: 0.0 };

    #[test]
    fn replays_what_is_not_acknowledged() {
        let mut p = Predictor::new(&[]);
        assert_eq!(p.input(1, EAST, EAST), None, "no snapshot yet");
        p.snapshot(1, 0, me(0.0, true));
        assert_eq!(p.position(), Some(Vec2::new(MOVE_SPEED, 20.0)), "input 1 is still unacked");
        p.input(2, EAST, EAST);
        assert_eq!(p.position(), Some(Vec2::new(2.0 * MOVE_SPEED, 20.0)));
        p.snapshot(2, 1, me(MOVE_SPEED, true));
        assert_eq!(p.position(), Some(Vec2::new(2.0 * MOVE_SPEED, 20.0)), "1 acked, 2 replayed");
    }

    /// The server's answer for an acked input is compared with what was
    /// predicted for it, once.
    #[test]
    fn scores_each_acked_input_once() {
        let mut p = Predictor::new(&[]);
        p.snapshot(1, 0, me(0.0, true));
        p.input(1, EAST, EAST);
        p.input(2, EAST, EAST);
        p.snapshot(2, 1, me(MOVE_SPEED, true));
        p.snapshot(3, 1, me(MOVE_SPEED, true));
        assert_eq!((p.stats().checked, p.stats().exact), (1, 1));
        p.snapshot(4, 2, me(2.0 * MOVE_SPEED + 1.0, true)); // the server disagrees
        let s = p.stats();
        assert_eq!((s.checked, s.exact, s.max_err), (2, 1, 1.0));
    }

    /// The same clamps the server applies: a long vector is a unit step, a
    /// non-finite one is no step, a wall stops it.
    #[test]
    fn steps_as_the_server_does() {
        let mut p = Predictor::new(&ARENA_WALLS);
        p.snapshot(1, 0, Some(PlayerState { id: 1, pos: Vec2::new(-15.0, 0.0), health: 100, alive: true }));
        p.input(1, Vec2::new(10.0, 0.0), EAST);
        assert_eq!(p.position(), Some(Vec2::new(-10.0, 0.0)));
        p.input(2, Vec2::new(f32::NAN, 0.0), EAST);
        assert_eq!(p.position(), Some(Vec2::new(-10.0, 0.0)), "refused by the server: no step");
        p.input(3, EAST, EAST);
        assert_eq!(p.position(), Some(Vec2::new(-10.0, 0.0)), "the pillar's face is solid");
    }

    /// Dead in the snapshot: nothing predicted, nothing scored, and an older
    /// snapshot arriving late changes nothing.
    #[test]
    fn a_dead_or_older_snapshot_predicts_nothing() {
        let mut p = Predictor::new(&[]);
        p.snapshot(5, 3, me(1.0, false));
        assert_eq!(p.input(4, EAST, EAST), None);
        assert_eq!(p.position(), Some(Vec2::new(1.0, 20.0)));
        p.snapshot(6, 4, me(7.0, true));
        assert_eq!(p.stats().checked, 0);
        p.snapshot(5, 4, me(99.0, true));
        assert_eq!(p.position(), Some(Vec2::new(7.0, 20.0)), "overtaken: ignored");
    }

    /// At MAX_PENDING unacked inputs every one is still folded; one more and
    /// the oldest is gone, from the replay and from the score.
    #[test]
    fn past_max_pending_the_oldest_input_is_dropped() {
        // East, west, east, ... — the arena's bounds never touched; any input
        // lost shows as a step out of place.
        let mut p = Predictor::new(&[]);
        p.snapshot(1, 0, me(0.0, true));
        let n = MAX_PENDING as u32;
        for seq in 1..=n {
            p.input(seq, Vec2::new(if seq % 2 == 1 { 1.0 } else { -1.0 }, 0.0), EAST);
        }
        assert_eq!(p.position(), Some(Vec2::new(0.0, 20.0)), "at the cap: all folded");
        p.input(n + 1, Vec2::new(0.0, 0.0), EAST);
        assert_eq!(p.position(), Some(Vec2::new(-MOVE_SPEED, 20.0)), "past it: input 1 (east) dropped");
        p.snapshot(2, 1, me(MOVE_SPEED, true));
        assert_eq!(p.stats().checked, 0, "input 1's prediction was dropped too");
        p.snapshot(3, 2, me(0.0, true));
        assert_eq!((p.stats().checked, p.stats().exact), (1, 1));
    }

    /// Pooled scores: counts add, the worst error is the worse one, the
    /// mean is over every check; no checks is a mean of 0, not NaN.
    #[test]
    fn scores_pool_and_average() {
        assert_eq!(PredictStats::default().mean_err(), 0.0);
        let mut a = PredictStats { checked: 2, exact: 1, max_err: 1.0, sum_err: 1.0 };
        a.add(&PredictStats { checked: 2, exact: 2, max_err: 0.5, sum_err: 1.0 });
        assert_eq!(a, PredictStats { checked: 4, exact: 3, max_err: 1.0, sum_err: 2.0 });
        assert_eq!(a.mean_err(), 0.5);
    }
}
