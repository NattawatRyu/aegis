//! G-10 stale-tick guard
//! STOPS: a client choosing which picture its shots are judged against.
//!        Every shot is judged in the frame of the snapshot its input claims
//!        (`crate::history`), so without a bound a client could claim:
//!          - one older than the history kept — no frame, so no evidence at
//!            all: every aim and reaction detector blind;
//!          - one from before it joined — the frame of whoever held its id
//!            before;
//!          - an older one than it already claimed — picking, shot by shot,
//!            whichever past picture flatters it.
//! HOW:   per player, a floor: the tick it was admitted, raised to every
//!        tick it claims. An input must claim a tick in
//!        [floor, now] and within `HISTORY` of now.
//! EDGE:  now - seen == HISTORY - 1 -> accepted | == HISTORY -> refused |
//!        seen == floor -> accepted (the same snapshot again) | floor - 1 ->
//!        refused | seen > now -> refused (no proof can exist for it; this
//!        holds even if the proof key leaked)
//!
//! Stage guard: runs after `tick_proof`, before the pipeline. The price is
//! that a player with a round trip over `HISTORY` ticks (533 ms) cannot
//! play. A client may still claim a picture up to that old, consistently —
//! it then plays as if that laggy, with every hit resolved where enemies
//! really are, which only costs it.

use super::RejectReason;
use crate::history::HISTORY;
use aegis_protocol::PlayerId;

pub struct StaleTick {
    floor: Box<[u32; 256]>,
}

impl Default for StaleTick {
    fn default() -> Self {
        Self { floor: Box::new([0; 256]) }
    }
}

impl StaleTick {
    /// `player` was admitted on `tick`: it can claim nothing older, whoever
    /// held the id before.
    pub fn admitted(&mut self, player: PlayerId, tick: u32) {
        self.floor[player as usize] = tick;
    }

    /// An input from `player` on server tick `now` claims snapshot `seen`.
    /// Accepted, it raises the floor to `seen`.
    pub fn check(&mut self, player: PlayerId, now: u32, seen: u32) -> Result<(), RejectReason> {
        let floor = &mut self.floor[player as usize];
        if seen < *floor || seen > now || now - seen >= HISTORY {
            return Err(RejectReason::StaleTick);
        }
        *floor = seen;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted_at(tick: u32) -> StaleTick {
        let mut g = StaleTick::default();
        g.admitted(1, tick);
        g
    }

    #[test]
    fn history_edge() {
        let now = 100;
        assert_eq!(admitted_at(0).check(1, now, now - (HISTORY - 1)), Ok(()));
        assert_eq!(admitted_at(0).check(1, now, now - HISTORY), Err(RejectReason::StaleTick));
        assert_eq!(admitted_at(0).check(1, now, now), Ok(()));
        assert_eq!(admitted_at(0).check(1, now, now + 1), Err(RejectReason::StaleTick));
    }

    #[test]
    fn never_older_than_already_claimed() {
        let mut g = admitted_at(0);
        assert_eq!(g.check(1, 50, 45), Ok(()));
        assert_eq!(g.check(1, 51, 45), Ok(()), "the same snapshot again");
        assert_eq!(g.check(1, 52, 44), Err(RejectReason::StaleTick), "went back a tick");
        assert_eq!(g.check(1, 52, 46), Ok(()));
    }

    /// A reused id starts at its own admission, not its predecessor's floor
    /// either way: an old floor neither blocks it nor lets it reach back.
    #[test]
    fn never_older_than_its_own_admission() {
        let mut g = admitted_at(0);
        assert_eq!(g.check(1, 60, 59), Ok(()));
        g.admitted(1, 70); // someone new holds id 1 from tick 70
        assert_eq!(g.check(1, 72, 69), Err(RejectReason::StaleTick), "the old holder's picture");
        assert_eq!(g.check(1, 72, 70), Ok(()));
    }

    #[test]
    fn players_are_independent() {
        let mut g = admitted_at(0);
        g.admitted(2, 0);
        assert_eq!(g.check(1, 50, 50), Ok(()));
        assert_eq!(g.check(2, 50, 40), Ok(()));
    }
}
