//! Aegis programmable bots.
//!
//! One [`Bot`] per file, mirroring the `guards/` layout: [`honest`] plus one
//! cheat per module. A bot speaks the wire protocol ([`ClientMsg`]) and knows
//! nothing about the server internals — it only sees what the server sends it
//! (its [`BotCtx::snapshot`]), exactly like a real client.
//!
//! ETHICS: every bot here is meant to be pointed at a server *you run*. That is
//! the whole design of the lab — attack yourself, measure the defense.

use aegis_protocol::{encode, ClientMsg, PlayerId, PlayerState, Vec2, PROTOCOL_VERSION};

pub mod aimbot;
pub mod badversion;
pub mod flood;
pub mod garbage;
pub mod honest;
pub mod nan;
pub mod replay;
pub mod speedhack;

/// What a bot sees before deciding this tick — the same information a real
/// client has. An aimbot exploits `snapshot`; culling it (pillar D) is what
/// takes that power away.
pub struct BotCtx<'a> {
    pub tick: u32,
    pub my_id: PlayerId,
    pub snapshot: &'a [PlayerState],
}

pub trait Bot {
    fn name(&self) -> &'static str;

    /// The handshake sent once, before tick 1. Default is a legal join; a bot
    /// attacking the version guard overrides it.
    fn join(&self) -> ClientMsg {
        ClientMsg::Join { name: self.name().into(), protocol: PROTOCOL_VERSION }
    }

    /// Messages to send this tick. Most bots send 0 or 1; a flooder sends many.
    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg>;

    /// The raw datagrams that go on the wire this tick. Default: `act`,
    /// encoded. Only a bot attacking the decoder itself (sending bytes that
    /// are not a `ClientMsg` at all) needs to override this.
    fn datagrams(&mut self, ctx: &BotCtx) -> Vec<Vec<u8>> {
        self.act(ctx).iter().map(encode).collect()
    }
}

/// This bot's own position from the last snapshot, if present.
pub fn my_pos(ctx: &BotCtx) -> Option<Vec2> {
    ctx.snapshot.iter().find(|p| p.id == ctx.my_id).map(|p| p.pos)
}

fn dist2(a: Vec2, b: Vec2) -> f32 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    dx * dx + dy * dy
}

/// Nearest alive enemy in the snapshot.
pub fn nearest_enemy<'a>(ctx: &'a BotCtx) -> Option<&'a PlayerState> {
    let me = my_pos(ctx)?;
    ctx.snapshot
        .iter()
        .filter(|p| p.id != ctx.my_id && p.alive)
        .min_by(|a, b| dist2(me, a.pos).total_cmp(&dist2(me, b.pos)))
}

/// Unit vector pointing from `from` to `to`. Falls back to +x if coincident.
pub fn unit_towards(from: Vec2, to: Vec2) -> Vec2 {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let l = (dx * dx + dy * dy).sqrt();
    if l > 0.0 {
        Vec2::new(dx / l, dy / l)
    } else {
        Vec2::new(1.0, 0.0)
    }
}
