//! Aegis programmable bots.
//!
//! One [`Bot`] per file, mirroring the `guards/` layout: [`honest`] plus one
//! cheat per module. A bot speaks the wire protocol ([`ClientMsg`]) and knows
//! nothing about the server internals — it only sees what the server sends it
//! (its [`BotCtx::snapshot`]), exactly like a real client.
//!
//! ETHICS: every bot here is meant to be pointed at a server *you run*. That is
//! the whole design of the lab — attack yourself, measure the defense.

use aegis_protocol::{frame, ClientMsg, PlayerId, PlayerState, Vec2, PROTOCOL_VERSION};

pub mod aimbot;
pub mod badversion;
pub mod burst;
pub mod camper;
pub mod direct;
pub mod esp;
pub mod flood;
pub mod garbage;
pub mod honest;
pub mod humanized;
pub mod joinflood;
pub mod nan;
pub mod reflect;
pub mod replay;
pub mod rusher;
pub mod speedhack;
pub mod spoof;
pub mod zeroflood;

/// What a bot sees before deciding this tick — the same information a real
/// client has. `snapshot` is the server's view for this player (pillar D
/// culling): only players it can see, so ESP has nothing behind a wall to use.
pub struct BotCtx<'a> {
    pub tick: u32,
    pub my_id: PlayerId,
    /// The session token from this bot's `Joined` (`NO_TOKEN` if it has none).
    pub token: u64,
    pub snapshot: &'a [PlayerState],
}

pub trait Bot {
    fn name(&self) -> &'static str;

    /// The handshake sent once, before tick 1. Default is a legal join; a bot
    /// attacking the version guard overrides it.
    fn join(&self) -> ClientMsg {
        ClientMsg::Join { name: self.name().into(), protocol: PROTOCOL_VERSION, cookie: None }
    }

    /// Messages to send this tick. Most bots send 0 or 1; a flooder sends many.
    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg>;

    /// The raw datagrams that go on the wire this tick. Default: `act`, each
    /// framed with the bot's session token. Only a bot attacking the decoder
    /// itself (sending bytes that are not a `ClientMsg` at all) needs to
    /// override this.
    fn datagrams(&mut self, ctx: &BotCtx) -> Vec<Vec<u8>> {
        self.act(ctx).iter().map(|m| frame(ctx.token, m)).collect()
    }

    /// The bot (by name) whose source address this bot's per-tick datagrams
    /// carry — a forged source. `None`, the default, sends from its own. The
    /// join always goes out from the bot's own address.
    fn impersonates(&self) -> Option<&'static str> {
        None
    }

    /// Whether this bot sends straight to the origin's address instead of the
    /// address it was given (the relay's) — it found the origin somehow.
    /// Without a relay the two are the same address.
    fn bypasses_relay(&self) -> bool {
        false
    }

    /// How many source ports this bot sends from (all on its one IP). Index
    /// 0 is its own address, where its join goes and its replies come back.
    fn sources(&self) -> u16 {
        1
    }

    /// This tick's datagrams, each with the source port index it leaves from.
    /// Default: every `datagrams` entry from port 0. Only a bot that uses
    /// more than one port overrides this.
    fn routed(&mut self, ctx: &BotCtx) -> Vec<(u16, Vec<u8>)> {
        self.datagrams(ctx).into_iter().map(|d| (0, d)).collect()
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

/// Deterministic noise in [-1, 1] (xorshift32), so bots with "human" error
/// still produce byte-identical runs. `state` must be non-zero.
pub fn jitter(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x as f32 / u32::MAX as f32 * 2.0 - 1.0
}

/// `v` rotated by `a` radians.
pub fn rotate(v: Vec2, a: f32) -> Vec2 {
    let (s, c) = a.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
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
