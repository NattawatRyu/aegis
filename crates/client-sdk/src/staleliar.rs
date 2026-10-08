//! StaleLiar — the triggerbot that claims to be laggier than it is.
//!
//! Every shot is judged in the picture its input claims (protocol v4), and
//! a proof cannot be forged for a snapshot not yet sent — but one for an
//! *older* snapshot it did get is real. So this bot acts on the newest
//! snapshot, like a [`TriggerBot`], and stamps each input with the one
//! `behind` ticks older, with that one's true proof.
//!
//! What it buys: an enemy it fires on the moment it appears is not yet in
//! the claimed picture. Its first shot is scored against an older picture
//! the enemy is missing from, and starts a run of fire there; by the time
//! the enemy is in the claimed picture the bot is mid-run, so the shot
//! reads as prefire and is not timed. An instant reaction is hidden, and it
//! looks like a laggy player who sprays corners.
//!
//! The one place that does not work is a respawn: a shot chosen while the
//! claimed picture shows it dead fires nothing (`Server::end_tick`), so its
//! run can only start on the first claimed picture it is alive in, where
//! every enemy in sight is new — an instant 0. So it also holds fire for
//! [`HOLD`] claimed ticks after each respawn: slow where it is watched,
//! instant everywhere else.
//!
//! What stops part of it: claims no older than the server keeps history for
//! (`stale_tick`), and never older than its last claim. [`StaleLiar::ancient`]
//! claims past the history and is refused every tick.

use std::collections::VecDeque;

use super::{triggerbot::TriggerBot, Bot, BotCtx};
use aegis_protocol::ClientMsg;

/// Claimed snapshots in a row it must be alive in before it fires: it first
/// fires on the 5th, so a respawn engagement reads 4 ticks — one over the
/// reaction detector's FAST_TICKS.
pub const HOLD: usize = 5;

pub struct StaleLiar {
    name: &'static str,
    behind: usize,
    trigger: TriggerBot,
    /// (tick, proof, alive in it) of the snapshots it has had, newest last.
    had: VecDeque<(u32, u32, bool)>,
}

impl StaleLiar {
    /// Claims the snapshot `behind` ticks older than the one it acts on, or
    /// its oldest until it has had that many.
    pub fn new(behind: usize) -> Self {
        Self::named("staleliar", behind)
    }

    /// Claims 20 ticks back: past the 16 the server keeps.
    pub fn ancient() -> Self {
        Self::named("ancient", 20)
    }

    /// The same, under its own name (to tell several apart in one run).
    pub fn named(name: &'static str, behind: usize) -> Self {
        Self { name, behind, trigger: TriggerBot::with_seed(0x6C8E_9CF5), had: VecDeque::new() }
    }
}

impl Bot for StaleLiar {
    fn name(&self) -> &'static str {
        self.name
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        let alive = ctx.snapshot.iter().any(|p| p.id == ctx.my_id && p.alive);
        self.had.push_back((ctx.tick, ctx.proof, alive));
        if self.had.len() > self.behind + HOLD {
            self.had.pop_front();
        }
        // The claimed snapshot, and whether it and the HOLD - 1 before it all
        // show it alive (too little history counts as a fresh respawn).
        let at = self.had.len().saturating_sub(self.behind + 1);
        let (old_tick, old_proof, _) = self.had[at];
        let settled = at + 1 >= HOLD && self.had.range(at + 1 - HOLD.min(at + 1)..=at).all(|&(.., a)| a);
        let mut out = self.trigger.act(ctx);
        if let ClientMsg::Input { tick, proof, shoot, .. } = &mut out[0] {
            (*tick, *proof) = (old_tick, old_proof);
            *shoot &= settled;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{PlayerState, Vec2};

    /// What it sends on `tick`, alive or not in that snapshot, with an enemy
    /// in sight: (claimed tick, proof, shoot).
    fn act(b: &mut StaleLiar, tick: u32, alive: bool) -> (u32, u32, bool) {
        let snap = [
            PlayerState { id: 1, pos: Vec2::ZERO, health: 100, alive },
            PlayerState { id: 2, pos: Vec2::new(0.0, -10.0), health: 100, alive: true },
        ];
        match b.act(&BotCtx { tick, proof: tick * 7, my_id: 1, token: 0, snapshot: &snap })[0] {
            ClientMsg::Input { tick, proof, shoot, .. } => (tick, proof, shoot),
            _ => panic!("expected Input"),
        }
    }

    /// It claims what it saw `behind` ago with that snapshot's own proof —
    /// the oldest it has, until it has that many — and holds fire until the
    /// claimed picture has shown it alive for HOLD ticks.
    #[test]
    fn claims_the_snapshot_behind_with_its_real_proof() {
        let mut b = StaleLiar::new(3);
        let got: Vec<(u32, u32, bool)> = (10..18).map(|t| act(&mut b, t, true)).collect();
        let claims = [10, 10, 10, 10, 11, 12, 13, 14];
        let want: Vec<(u32, u32, bool)> = claims.iter().map(|&c| (c, c * 7, c >= 10 + HOLD as u32 - 1)).collect();
        assert_eq!(got, want);
    }

    /// A respawn in the claimed picture: dead there, it holds fire, and for
    /// HOLD claimed ticks after — though it sees an enemy all along.
    #[test]
    fn holds_fire_after_a_respawn_it_claims() {
        let mut b = StaleLiar::new(2);
        for t in 1..=10 {
            act(&mut b, t, true);
        }
        act(&mut b, 11, false); // dead on 11, alive again from 12
        let got: Vec<(u32, bool)> = (12..=20).map(|t| act(&mut b, t, true)).map(|(c, _, s)| (c, s)).collect();
        // Claims 10..=18: 10 is before the death, 11 shows it dead, 12..=14
        // are its first HOLD - 1 alive ones.
        let want: Vec<(u32, bool)> = (10..=18).map(|c| (c, c == 10 || c >= 11 + HOLD as u32)).collect();
        assert_eq!(got, want);
    }
}
