//! SpoofBot — an off-path attacker who knows a victim's address and forges
//! datagrams from it. It joins legally from its own address (it is a player
//! too), then every tick sends a burst of inputs whose *source* is the victim's
//! address: the harness routes them out of the victim's socket, which is what
//! a forged source address looks like on the wire.
//!
//! Two goals, both of which the server must defeat:
//!   - act as the victim (drag it around, put junk on its record so a detector
//!     flags the victim instead of the attacker);
//!   - starve the victim: if the server budgets datagrams per source IP before
//!     knowing who sent them, a burst from the victim's IP spends the victim's
//!     budget and its real input is dropped.
//!
//! It cannot see the victim's session token (it is not on the path), so the
//! best it has is its own.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

/// Forged datagrams per tick — past the per-IP cap, so the starvation attempt
/// is real.
pub const PER_TICK: usize = 50;

/// The bot whose address it forges.
pub const VICTIM: &str = "honest";

/// Forged seqs start far ahead of any real client, so if one were ever
/// accepted the replay guard would then refuse the victim's own inputs.
pub const SEQ_BASE: u32 = 1_000_000;

pub struct SpoofBot {
    seq: u32,
}

impl SpoofBot {
    pub fn new() -> Self {
        Self { seq: SEQ_BASE }
    }
}

impl Default for SpoofBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for SpoofBot {
    fn name(&self) -> &'static str {
        "spoof"
    }

    fn impersonates(&self) -> Option<&'static str> {
        Some(VICTIM)
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        (0..PER_TICK)
            .map(|_| {
                self.seq += 1;
                ClientMsg::Input {
                    seq: self.seq,
                    tick: ctx.tick,
                    proof: ctx.proof,
                    move_dir: Vec2::new(-1.0, 0.0),
                    aim: Vec2::new(-1.0, 0.0),
                    shoot: false,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forges_a_burst_past_the_cap_with_seqs_ahead_of_any_client() {
        let mut b = SpoofBot::new();
        let out = b.act(&BotCtx { tick: 1, my_id: 1, token: 0, proof: 0, snapshot: &[] });
        assert_eq!(out.len(), PER_TICK);
        assert!(matches!(out[0], ClientMsg::Input { seq, .. } if seq > SEQ_BASE));
        assert_eq!(b.impersonates(), Some(VICTIM));
    }
}
