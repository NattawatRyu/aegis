//! TickLiar — the triggerbot that read the reaction detector's source.
//!
//! Reaction is timed from the snapshot an input says it was chosen on
//! (protocol v4), not from when the input arrived — so latency no longer
//! hides an instant shot. This bot is a [`TriggerBot`] that claims each input
//! was chosen [`LIE`] ticks after the snapshot it really had: an instant shot
//! stamped to read as a human's reaction.
//!
//! It cannot stamp the claimed tick's proof — only the server can make one,
//! and it has not been sent that snapshot yet — so it echoes the one it has.
//! Every input it sends is refused (`bad_tick_proof`): it never moves and
//! never fires. A liar that told the truth when not shooting would only
//! lose its shots instead of everything.

use super::{triggerbot::TriggerBot, Bot, BotCtx};
use aegis_protocol::ClientMsg;

/// How many ticks newer than its snapshot it claims: the honest bot's
/// fastest reaction, so a shot on sight would read as human.
pub const LIE: u32 = crate::honest::REACT_MIN;

pub struct TickLiar {
    trigger: TriggerBot,
}

impl TickLiar {
    pub fn new() -> Self {
        Self { trigger: TriggerBot::with_seed(0x1656_67B1) }
    }
}

impl Default for TickLiar {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for TickLiar {
    fn name(&self) -> &'static str {
        "tickliar"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.trigger.act(ctx);
        if let ClientMsg::Input { tick, .. } = &mut out[0] {
            *tick += LIE;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{PlayerState, Vec2};

    /// It fires on sight like the triggerbot, claiming a snapshot LIE ticks
    /// newer than the one it has, and echoing that one's proof.
    #[test]
    fn claims_a_newer_snapshot_with_the_proof_of_the_old() {
        let snap = [
            PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive: true },
            PlayerState { id: 2, pos: Vec2::new(0.0, -10.0), health: 100, alive: true },
        ];
        let out = TickLiar::new().act(&BotCtx { tick: 40, proof: 0xABCD, my_id: 1, token: 0, snapshot: &snap });
        match out[0] {
            ClientMsg::Input { tick, proof, shoot, .. } => assert_eq!((tick, proof, shoot), (40 + LIE, 0xABCD, true)),
            _ => panic!("expected Input"),
        }
    }
}
