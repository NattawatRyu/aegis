//! G4 input-rate guard
//! STOPS: speedhack / action-spam via more inputs than ticks, and a client
//!        banking inputs to spend later
//! HOW:   inputs wait in the player's [`crate::inbox::Inbox`] lane, and the
//!        sim folds at most one per tick, so flooding buys no extra
//!        movement. The lane holds at most `DEPTH`; an input arriving to a
//!        full one is rejected.
//! EDGE:  `DEPTH - 1` waiting -> accepted | `DEPTH` waiting -> rejected |
//!        the next pop makes room again.
//!
//! Stage guard (B1): runs after the pipeline, at the queue. Before B1 it
//! refused a second input on one tick, which on a jittery link refused an
//! honest player's — two inputs land together and the next tick none does.
//! `DEPTH` 6 rides out 3 ticks (100 ms) of jitter on a lossy link; what
//! waits longer lags only the player who sent it.
//!
//! 6, not the 4 B1 started with (B2, `aegis-harness net`, rtt 4): once
//! lost inputs come back in the next datagrams, each loss on an empty lane
//! leaves one more waiting, and with 3 ticks of jitter up to 4 datagrams
//! land together on top of that backlog. At 4, jitter 2–3 with 2–10% loss
//! refused up to 1.1% of an honest player's inputs; at 6 none, in every
//! cell (8 was no different).

use super::RejectReason;

/// Inputs one player may have waiting.
pub const DEPTH: usize = 6;

/// May an input join a lane with `waiting` inputs in it?
pub fn check(waiting: usize) -> Result<(), RejectReason> {
    if waiting >= DEPTH {
        return Err(RejectReason::RateExceeded);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::Inbox;
    use crate::ClientInput;
    use aegis_protocol::Vec2;

    #[test]
    fn depth_edge() {
        assert_eq!(check(DEPTH - 1), Ok(()));
        assert_eq!(check(DEPTH), Err(RejectReason::RateExceeded));
    }

    /// A flood of 100 inputs in one tick: `DEPTH` wait, the rest are
    /// refused, and each tick folds exactly one of them.
    #[test]
    fn a_flood_waits_at_most_depth_and_drains_one_a_tick() {
        let mut b = Inbox::default();
        let mut refused = 0;
        for seq in 1..=100 {
            match check(b.len(1)) {
                Ok(()) => {
                    b.push(1, ClientInput { seq, tick: 1, move_dir: Vec2::ZERO, aim: Vec2::ZERO, shoot: false }, 1)
                }
                Err(_) => refused += 1,
            }
        }
        assert_eq!((b.len(1), refused), (DEPTH, 100 - DEPTH));
        assert_eq!(b.pop_all(1).len(), 1);
        assert_eq!(check(b.len(1)), Ok(()), "a pop makes room");
    }
}
