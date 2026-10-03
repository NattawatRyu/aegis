//! Snapshot culling policy: what each player is sent, and with how much
//! margin.
//!
//! STOPS: wallhack / ESP (an enemy behind a wall is never sent), without
//!        making a laggy honest player blind: an enemy about to come round a
//!        corner is sent early enough that the client sees it on time.
//! HOW:   each player's view is [`Sim::view_within`] its *margin*: how many
//!        ticks behind the server that player acts, measured from its inputs
//!        (server tick minus the tick of the snapshot the input says it was
//!        chosen on), capped at [`MAX_MARGIN_TICKS`].
//! EDGE:  lag 0 -> the exact view | lag ≤ cap -> margin = lag | lag > cap
//!        (or a client that claims an ancient tick) -> margin = cap, never
//!        more. The cap is the whole defense against fake lag: without it a
//!        client could claim a huge lag and be sent everything — a wallhack
//!        the server hands out.
//!
//! The margin leaks, on purpose: players who are not visible yet but could
//! be within the margin. `aegis-harness cull` measures that leak and the
//! pop-in it prevents. Over 250 honest lobbies in `ARENA_WALLS`:
//!
//!   margin 0: leaks 0% of walled pairs; late for 6.7% of visible pairs at
//!             lag 1, 17.0% at lag 3
//!   margin 1: leaks 82.6%;  late 0.1% at lag 1
//!   margin 2: leaks 99.0%
//!   margin 3: leaks 100% — culling off
//!
//! At `MOVE_SPEED` 5 a tick both sides walk round any of these walls inside
//! one tick, so even one tick of margin hands ESP most of what culling hid.
//! The leak depends on reach (step x margin) alone — `aegis-harness
//! cull-scale` on the same lobbies: reach 5 -> 82.8%, 1 -> 18.3%, 0.5 ->
//! 7.8%, 0.25 -> 3.4%, 0.1 -> 1.7%. A margin is only cheap in a world where
//! 100 ms of movement is small next to a wall.
//! DECIDED 2026-10-03: the server sends margin 0 ([`Sim::view`]) — ESP gets
//! nothing, honest players eat the pop-in. `Cull` is NOT wired in; it stays
//! as lab code for a game whose scale makes a margin cheap, which it can
//! measure on its own map with `cull-scale`. [`MAX_MARGIN_TICKS`] is unused
//! by the server.

use std::collections::BTreeMap;

use aegis_protocol::{PlayerId, PlayerState};

use crate::Sim;

/// Most ticks of margin any player gets, whatever lag it shows. Unsettled:
/// measured at 3 it leaks every walled player (see the module doc).
pub const MAX_MARGIN_TICKS: u32 = 3;

pub struct Cull {
    cap: u32,
    /// Ticks behind the server each player's last accepted input was.
    lag: BTreeMap<PlayerId, u32>,
    /// This tick's view per player, computed once in [`Cull::refresh`].
    views: BTreeMap<PlayerId, Vec<PlayerState>>,
}

impl Default for Cull {
    fn default() -> Self {
        Self::new()
    }
}

impl Cull {
    pub fn new() -> Self {
        Self::with_cap(MAX_MARGIN_TICKS)
    }

    /// A different cap — for the lab's sweep that measures where it belongs,
    /// not for a live server.
    pub fn with_cap(cap: u32) -> Self {
        Self { cap, lag: BTreeMap::new(), views: BTreeMap::new() }
    }

    /// An accepted input from `player` at server tick `now`, chosen on the
    /// snapshot of `client_tick`. A tick from the future reads as no lag.
    pub fn note(&mut self, player: PlayerId, now: u32, client_tick: u32) {
        self.lag.insert(player, now.saturating_sub(client_tick));
    }

    /// The margin `player` is given: its lag, never more than the cap.
    pub fn margin(&self, player: PlayerId) -> u32 {
        self.lag.get(&player).copied().unwrap_or(0).min(self.cap)
    }

    /// Compute every player's view of `sim` for this tick.
    pub fn refresh(&mut self, sim: &Sim) {
        self.views = sim.snapshot().iter().map(|p| (p.id, sim.view_within(p.id, self.margin(p.id)))).collect();
    }

    /// What `player` is sent this tick. Empty for one not in the world.
    pub fn view(&self, player: PlayerId) -> Vec<PlayerState> {
        self.views.get(&player).cloned().unwrap_or_default()
    }

    /// `player` left: its lag must not carry over to whoever gets its id.
    pub fn forget(&mut self, player: PlayerId) {
        self.lag.remove(&player);
        self.views.remove(&player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Wall;
    use aegis_protocol::Vec2;

    /// The cap at its edge — the fake-lag defense. Lag up to the cap is the
    /// margin; one past it, or a client claiming tick 0 a thousand ticks in,
    /// gets exactly the cap.
    #[test]
    fn margin_is_lag_up_to_the_cap_and_never_more() {
        let mut c = Cull::new();
        assert_eq!(c.margin(1), 0, "no input yet: no margin");
        for lag in 0..=MAX_MARGIN_TICKS {
            c.note(1, 100, 100 - lag);
            assert_eq!(c.margin(1), lag);
        }
        c.note(1, 100, 100 - MAX_MARGIN_TICKS - 1);
        assert_eq!(c.margin(1), MAX_MARGIN_TICKS);
        c.note(1, 1000, 0);
        assert_eq!(c.margin(1), MAX_MARGIN_TICKS);
        c.note(1, 100, 105); // from the future
        assert_eq!(c.margin(1), 0);
    }

    /// What the cap protects, end to end: a client claiming an ancient tick
    /// is sent no more than one at exactly the cap's lag — a player far
    /// behind a long wall stays hidden from it.
    #[test]
    fn fake_lag_buys_no_more_than_the_cap() {
        // a wall between x=4..6, y=-20..20: the cap's 3 steps (15 units) stay
        // below its ends, but it is short of the arena edge (50) so a long
        // enough walk does get round it
        let mut sim = Sim::with_walls(&[Wall::new(Vec2::new(4.0, -20.0), Vec2::new(6.0, 20.0))]);
        sim.spawn(1, Vec2::ZERO);
        sim.spawn(2, Vec2::new(10.0, 0.0));
        let mut honest = Cull::new();
        honest.note(1, 1000, 1000 - MAX_MARGIN_TICKS);
        let mut faker = Cull::new();
        faker.note(1, 1000, 0);
        honest.refresh(&sim);
        faker.refresh(&sim);
        assert_eq!(faker.view(1), honest.view(1));
        assert!(faker.view(1).iter().all(|p| p.id != 2), "fake lag saw through the wall");
        // whereas a cap of 30 (both walk to the arena edge, y=50, past the
        // wall's end) would have handed it over — the cap is what holds
        let mut loose = Cull::with_cap(30);
        loose.note(1, 1000, 0);
        loose.refresh(&sim);
        assert!(loose.view(1).iter().any(|p| p.id == 2), "test setup: a loose cap should leak here");
    }

    #[test]
    fn forget_drops_the_lag_so_a_reused_id_starts_clean() {
        let mut c = Cull::new();
        c.note(4, 50, 48);
        c.forget(4);
        assert_eq!(c.margin(4), 0);
    }
}
