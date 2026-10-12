//! Redundant — not a cheat: what a real client does with its inputs (v5).
//! Wraps any bot that plays like a client, and sends with each of its
//! inputs the last [`MAX_OLDER`] the server has not acknowledged, as
//! [`crate::conn::Connection`] does. A lost datagram then costs nothing as
//! long as one of the next two arrives.
//!
//! The bots that attack a guard's edge stay raw: their one frame per
//! datagram is the attack. Only for a bot that keeps the default
//! `datagrams` / `routed` — the copies are added to what `act` returns.
//!
//! It also predicts the bot's own position as a real client would (B3,
//! [`Predictor`]) and keeps score, so the lab measures prediction on the
//! same crowds, links and inputs it measures everything else on. The bot
//! itself still acts on the snapshot, not on the prediction.
//!
//! Told to interpolate (B4, [`Bot::interpolate`]), it acts on what an
//! [`Interpolator`] draws instead of the newest snapshot: the wrapped bot
//! sees the drawn picture and its inputs claim the drawn tick. Either way it
//! scores how steadily what it acts on advanced ([`Pace`]).

use super::interp::{Interpolator, Pace};
use super::predict::{PredictStats, Predictor};
use super::{Bot, BotCtx};
use aegis_protocol::movement::Wall;
use aegis_protocol::{ClientMsg, InputFrame, PlayerState, MAX_OLDER};

pub struct Redundant<B> {
    inner: B,
    /// The newest inputs sent and not yet acknowledged, oldest first.
    unacked: Vec<InputFrame>,
    /// The `ack` of the snapshot about to be acted on.
    ack: u32,
    predictor: Predictor,
    interp: Option<Interpolator>,
    /// Without an interpolator: the pace of the snapshots acted on, and the
    /// last one's tick.
    pace: Pace,
    shown: Option<u32>,
}

impl<B: Bot> Redundant<B> {
    /// `inner`, in an arena with `walls` (what its predictor steps against).
    pub fn new(inner: B, walls: &[Wall]) -> Self {
        Self {
            inner,
            unacked: Vec::new(),
            ack: 0,
            predictor: Predictor::new(walls),
            interp: None,
            pace: Pace::default(),
            shown: None,
        }
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
        let me = ctx.snapshot.iter().find(|p| p.id == ctx.my_id).copied();
        self.predictor.snapshot(ctx.tick, self.ack, me);
        // The lab acts once a tick: one tick passes per frame.
        // The drawn frame's (claim, offset), when interpolating.
        let (mut out, drawn) = match &mut self.interp {
            None => {
                self.pace.frame(self.shown.map(f64::from), f64::from(ctx.tick), 1.0, ctx.tick);
                self.shown = Some(ctx.tick);
                (self.inner.act(ctx), None)
            }
            Some(ip) => {
                let Some(v) = ip.advance(1.0) else { return Vec::new() };
                let ctx = BotCtx { tick: v.claim, proof: v.proof, snapshot: &v.players, ..*ctx };
                (self.inner.act(&ctx), Some((v.claim, v.offset)))
            }
        };
        for m in &mut out {
            let Some(&(mut own)) = m.frames().last() else { continue };
            // An input on the drawn frame says where it was drawn (v6).
            if let Some((_, offset)) = drawn.filter(|&(c, _)| c == own.tick) {
                own.offset = offset;
            }
            self.predictor.input(own.seq, own.move_dir, own.aim);
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
        self.ack = ack;
        self.unacked.retain(|f| f.seq > ack);
    }

    fn prediction(&self) -> Option<PredictStats> {
        Some(self.predictor.stats())
    }

    fn landed(&mut self, tick: u32, proof: u32, view: &[PlayerState]) {
        if let Some(ip) = &mut self.interp {
            ip.snapshot(tick, proof, view);
        }
        self.inner.landed(tick, proof, view)
    }

    fn interpolate(&mut self, delay: f64) {
        self.interp = Some(Interpolator::new(delay));
    }

    fn pace(&self) -> Option<Pace> {
        Some(self.interp.as_ref().map_or(self.pace, Interpolator::pace))
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
        let mut b = Redundant::new(HonestBot::new(), &[]);
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

    /// Interpolating, the wrapped bot acts on the drawn picture and its
    /// input claims the drawn tick, with that tick's proof — not the
    /// snapshot it was handed.
    #[test]
    fn interpolating_it_claims_what_it_draws() {
        let mut b = Redundant::new(HonestBot::new(), &[]);
        b.interpolate(2.0);
        let snap = [PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive: true }];
        for t in 1..=5 {
            b.landed(t, t * 100, &snap);
        }
        let ctx = BotCtx { tick: 5, proof: 500, my_id: 1, token: 9, snapshot: &snap };
        let f = b.act(&ctx)[0].frames()[0];
        assert_eq!((f.tick, f.proof), (3, 300));
        let p = b.pace().unwrap();
        assert_eq!(p.frames, 0, "the first frame is not judged");
        let mut raw = Redundant::new(HonestBot::new(), &[]);
        raw.landed(1, 100, &snap);
        assert_eq!(raw.act(&ctx)[0].frames()[0].tick, 5, "not interpolating: the snapshot handed over");
    }

    /// Its own frame is the wrapped bot's, untouched.
    #[test]
    fn its_own_frame_is_the_wrapped_bots() {
        let snap = [PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive: true }];
        let ctx = BotCtx { tick: 1, proof: 7, my_id: 1, token: 9, snapshot: &snap };
        let plain = HonestBot::new().act(&ctx);
        let wrapped = Redundant::new(HonestBot::new(), &[]).act(&ctx);
        assert_eq!(plain, wrapped, "nothing unacknowledged yet: the same message");
    }
}
