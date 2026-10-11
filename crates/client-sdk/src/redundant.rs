//! Redundant — not a cheat: what a real client does with its inputs (v5).
//! Wraps any bot that plays like a client, and sends with each of its
//! inputs the last [`MAX_OLDER`] the server has not acknowledged, as
//! [`crate::conn::Connection`] does. A lost datagram then costs nothing as
//! long as one of the next two arrives.
//!
//! The bots that attack a guard's edge stay raw: their one frame per
//! datagram is the attack. Only for a bot that keeps the default
//! `datagrams` / `routed` — the copies are added to what `act` returns.

use super::{Bot, BotCtx};
use aegis_protocol::{ClientMsg, InputFrame, MAX_OLDER};

pub struct Redundant<B> {
    inner: B,
    /// The newest inputs sent and not yet acknowledged, oldest first.
    unacked: Vec<InputFrame>,
}

impl<B: Bot> Redundant<B> {
    pub fn new(inner: B) -> Self {
        Self { inner, unacked: Vec::new() }
    }
}

impl<B: Bot> Bot for Redundant<B> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn join(&self) -> ClientMsg {
        self.inner.join()
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let mut out = self.inner.act(ctx);
        for m in &mut out {
            let Some(&own) = m.frames().last() else { continue };
            *m = ClientMsg::input_with(own, self.unacked.clone());
            self.unacked.push(own);
            if self.unacked.len() > MAX_OLDER {
                self.unacked.remove(0);
            }
        }
        out
    }

    fn impersonates(&self) -> Option<&'static str> {
        self.inner.impersonates()
    }

    fn bypasses_relay(&self) -> bool {
        self.inner.bypasses_relay()
    }

    fn taps(&self) -> Option<&'static str> {
        self.inner.taps()
    }

    fn overheard(&mut self, wire: &[u8]) {
        self.inner.overheard(wire)
    }

    fn acked(&mut self, ack: u32) {
        self.unacked.retain(|f| f.seq > ack);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::honest::HonestBot;
    use aegis_protocol::{PlayerState, Vec2};

    fn seqs(b: &mut Redundant<HonestBot>, tick: u32) -> Vec<u32> {
        let snap = [PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive: true }];
        let ctx = BotCtx { tick, proof: 0, my_id: 1, token: 9, snapshot: &snap };
        let out = b.act(&ctx);
        assert_eq!(out.len(), 1);
        out[0].frames().iter().map(|f| f.seq).collect()
    }

    /// The same inputs as the bot it wraps, plus what is unacknowledged:
    /// never more than MAX_OLDER along, pruned at and below the ack.
    #[test]
    fn carries_the_unacknowledged_and_prunes_on_ack() {
        let mut b = Redundant::new(HonestBot::new());
        assert_eq!(b.name(), "honest");
        assert_eq!(seqs(&mut b, 1), [1]);
        assert_eq!(seqs(&mut b, 2), [1, 2]);
        assert_eq!(seqs(&mut b, 3), [1, 2, 3]);
        assert_eq!(seqs(&mut b, 4), [2, 3, 4]);
        b.acked(3);
        assert_eq!(seqs(&mut b, 5), [4, 5]);
        b.acked(5);
        assert_eq!(seqs(&mut b, 6), [6]);
    }

    /// Its own frame is the wrapped bot's, untouched.
    #[test]
    fn its_own_frame_is_the_wrapped_bots() {
        let snap = [PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive: true }];
        let ctx = BotCtx { tick: 1, proof: 7, my_id: 1, token: 9, snapshot: &snap };
        let plain = HonestBot::new().act(&ctx);
        let wrapped = Redundant::new(HonestBot::new()).act(&ctx);
        assert_eq!(plain, wrapped, "nothing unacknowledged yet: the same message");
    }
}
