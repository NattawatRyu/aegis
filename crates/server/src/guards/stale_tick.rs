//! G-10 stale-tick guard
//! STOPS: a client choosing which picture its shots are judged against.
//!        Every shot is judged in the frame of the snapshot its input claims
//!        (`crate::history`), so without a bound a client could claim:
//!          - one older than the history kept — no frame, so no evidence at
//!            all: every aim and reaction detector blind;
//!          - one from before it joined — the frame of whoever held its id
//!            before;
//!          - an older one than its earlier input claimed — picking, shot by
//!            shot, whichever past picture flatters it.
//! HOW:   an input must claim a tick in [admission, now] and within
//!        `HISTORY` of now; and, in seq order, no older than the input
//!        before it and no newer than the input after it (its neighbours in
//!        [`crate::inbox::Inbox`]). When the input is taken to be folded in,
//!        `HISTORY` is checked again ([`fresh`]): it may have waited.
//! EDGE:  now - seen == HISTORY - 1 -> accepted | == HISTORY -> refused |
//!        seen == the earlier input's -> accepted (the same snapshot again) |
//!        one less -> refused | seen > now -> refused (no proof can exist for
//!        it; this holds even if the proof key leaked)
//!
//! Stage guard: runs after `tick_proof` and `replay`, before the pipeline.
//! The order is by seq, not arrival (B1): an input overtaken on the way
//! claims an older picture than the one that overtook it, and that is no
//! lie. The price is that a player with a round trip over `HISTORY` ticks
//! (533 ms) cannot play. A client may still claim a picture up to that old,
//! consistently — it then plays as if that laggy, with every hit resolved
//! where enemies really are, which only costs it.

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
    /// `below` / `above`: what its neighbours in seq order claimed, if known.
    pub fn check(
        &self,
        player: PlayerId,
        now: u32,
        seen: u32,
        below: Option<u32>,
        above: Option<u32>,
    ) -> Result<(), RejectReason> {
        let floor = self.floor[player as usize].max(below.unwrap_or(0));
        if seen < floor || above.is_some_and(|a| seen > a) {
            return Err(RejectReason::StaleTick);
        }
        fresh(now, seen)
    }
}

/// Is snapshot `seen` still in the history on tick `now`?
pub fn fresh(now: u32, seen: u32) -> Result<(), RejectReason> {
    if seen > now || now - seen >= HISTORY {
        return Err(RejectReason::StaleTick);
    }
    Ok(())
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
        let g = admitted_at(0);
        assert_eq!(g.check(1, now, now - (HISTORY - 1), None, None), Ok(()));
        assert_eq!(g.check(1, now, now - HISTORY, None, None), Err(RejectReason::StaleTick));
        assert_eq!(g.check(1, now, now, None, None), Ok(()));
        assert_eq!(g.check(1, now, now + 1, None, None), Err(RejectReason::StaleTick));
    }

    /// Taken after waiting: the same edge, on the tick it is folded in.
    #[test]
    fn fresh_edge() {
        assert_eq!(fresh(100, 100 - (HISTORY - 1)), Ok(()));
        assert_eq!(fresh(100, 100 - HISTORY), Err(RejectReason::StaleTick));
        assert_eq!(fresh(100, 101), Err(RejectReason::StaleTick));
    }

    #[test]
    fn never_older_than_the_input_before() {
        let g = admitted_at(0);
        assert_eq!(g.check(1, 51, 45, Some(45), None), Ok(()), "the same snapshot again");
        assert_eq!(g.check(1, 52, 44, Some(45), None), Err(RejectReason::StaleTick), "went back a tick");
        assert_eq!(g.check(1, 52, 46, Some(45), None), Ok(()));
    }

    /// Overtaken on the way: older than the input after it is fine, newer
    /// is not — seq order, not arrival order.
    #[test]
    fn never_newer_than_the_input_after() {
        let g = admitted_at(0);
        assert_eq!(g.check(1, 52, 44, Some(43), Some(45)), Ok(()));
        assert_eq!(g.check(1, 52, 45, Some(43), Some(45)), Ok(()));
        assert_eq!(g.check(1, 52, 46, Some(43), Some(45)), Err(RejectReason::StaleTick));
    }

    /// A reused id starts at its own admission, not its predecessor's floor
    /// either way: an old floor neither blocks it nor lets it reach back.
    #[test]
    fn never_older_than_its_own_admission() {
        let mut g = admitted_at(0);
        assert_eq!(g.check(1, 60, 59, None, None), Ok(()));
        g.admitted(1, 70); // someone new holds id 1 from tick 70
        assert_eq!(g.check(1, 72, 69, None, None), Err(RejectReason::StaleTick), "the old holder's picture");
        assert_eq!(g.check(1, 72, 70, None, None), Ok(()));
    }

    #[test]
    fn players_are_independent() {
        let mut g = admitted_at(0);
        g.admitted(2, 0);
        assert_eq!(g.check(1, 50, 50, None, None), Ok(()));
        assert_eq!(g.check(2, 50, 40, None, None), Ok(()));
    }
}
