//! G5 replay guard
//! STOPS: replay — resending a captured input, over and over
//! HOW:   an input whose seq was already seen (taken, older than taken, or
//!        waiting: [`crate::inbox::Inbox::is_late`]) is dropped either way;
//!        it changes nothing. A run of `STALE_RUN` of them in a row, with no
//!        new seq between, is a replay and is rejected — every one from
//!        then until a new seq arrives.
//! EDGE:  `STALE_RUN - 1` stale in a row -> dropped silently |
//!        `STALE_RUN`th -> rejected | a new seq resets the run.
//!
//! Stage guard (B1): runs after the tick proof, before the queue. Before B1
//! every stale seq was rejected, so a datagram merely overtaken on the way
//! went on an honest player's record. Overtaken ones come singly, or a few
//! at a time — never `STALE_RUN` in a row while new inputs are still
//! arriving — up to 3 ticks of jitter (100 ms), past which an honest
//! player could trip it.

use super::RejectReason;
use aegis_protocol::PlayerId;

/// Stale inputs in a row that make a replay.
pub const STALE_RUN: u8 = 4;

pub struct Replay {
    run: Box<[u8; 256]>,
}

impl Default for Replay {
    fn default() -> Self {
        Self { run: Box::new([0; 256]) }
    }
}

impl Replay {
    /// An input from `player` arrived, `late` or not. `Ok(true)`: new, go on.
    /// `Ok(false)`: stale, drop it unrecorded. `Err`: a replay.
    pub fn check(&mut self, player: PlayerId, late: bool) -> Result<bool, RejectReason> {
        let run = &mut self.run[player as usize];
        if !late {
            *run = 0;
            return Ok(true);
        }
        *run = run.saturating_add(1);
        if *run >= STALE_RUN {
            return Err(RejectReason::Replay);
        }
        Ok(false)
    }

    /// `player`'s session ended: whoever holds the id next starts clean.
    pub fn forget(&mut self, player: PlayerId) {
        self.run[player as usize] = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_edge() {
        let mut g = Replay::default();
        assert_eq!(g.check(1, false), Ok(true));
        for _ in 1..STALE_RUN {
            assert_eq!(g.check(1, true), Ok(false));
        }
        assert_eq!(g.check(1, true), Err(RejectReason::Replay));
        assert_eq!(g.check(1, true), Err(RejectReason::Replay), "still a replay");
    }

    #[test]
    fn a_new_seq_resets_the_run() {
        let mut g = Replay::default();
        for _ in 1..STALE_RUN {
            g.check(1, true).unwrap();
        }
        assert_eq!(g.check(1, false), Ok(true));
        assert_eq!(g.check(1, true), Ok(false));
    }

    #[test]
    fn forget_and_players_are_independent() {
        let mut g = Replay::default();
        for _ in 1..STALE_RUN {
            g.check(1, true).unwrap();
        }
        assert_eq!(g.check(2, true), Ok(false));
        g.forget(1);
        assert_eq!(g.check(1, true), Ok(false));
    }
}
