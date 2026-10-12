//! Aegis as a running game server — what an engine's client connects to.
//!
//! Everything else in the workspace drives the server in lockstep, one tick
//! when a test says so. This runs it against the clock:
//!
//! ```text
//! client ──UDP──> relay (public address; Noise NK hands out sessions) ──> origin (30 Hz, private)
//!                                                                          └─> telemetry -> Monitor -> alerts
//! ```
//!
//! - **Origin**: [`NetServer::run_tick`] at [`aegis_protocol::TICK_HZ`], in
//!   the walled arena ([`ARENA_WALLS`]). Its address is never told to anyone.
//! - **Relay**: the only address clients see. Behind it the origin hears
//!   nothing but sealed, MAC'd envelopes. With `direct` there is no relay
//!   and clients speak to the origin unsealed — for debugging only.
//! - **Sessions**: the relay is its own backend. A client that knows only
//!   the relay's public key ([`Game::relay_public`]) runs the Noise handshake
//!   (`aegis_protocol::noise`) and is handed its keys. [`Game::mint`] issues
//!   keys directly, as a game's own backend would.
//! - **Detection**: every record the server writes is streamed into an
//!   online [`Monitor`] each tick, and then dropped. Nothing grows with how
//!   long the game runs. [`Game::tick`] returns the alerts.
//! - **Opponents**: the lab's bots ([`bots`]), each on its own thread and
//!   its own socket, through the relay like any player.

pub mod bots;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};

use aegis_detector::{Alert, Monitor};
use aegis_protocol::noise::RelayStatic;
use aegis_protocol::{mint_connect, ClientKeys, EdgeKey, LinkKey, PlayerId, Vec2};
use aegis_relay::Relay;
use aegis_server::{NetServer, Server, ARENA_WALLS};
use aegis_telemetry::{Outcome, Record};

use bots::{BotKind, BotThread};

/// How long a session handed out over Noise admits Joins.
pub const SESSION_TTL_SECS: u64 = 3600;

/// Spawn points: this many, on a circle around the arena's middle.
const SPAWNS: usize = 16;
const SPAWN_RADIUS: f32 = 20.0;

pub struct Config {
    /// What clients are told: the relay's public address (the origin's, when
    /// `direct`).
    pub listen: SocketAddr,
    /// No relay: clients speak to the origin, unsealed.
    pub direct: bool,
    pub bots: Vec<BotKind>,
    /// Seeds the bots' hands.
    pub seed: u32,
}

/// One player's session as the server saw it. Starts over when the session
/// ends (`Left`): a reused id is someone else.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    pub accepted: u32,
    pub rejected: u32,
    pub shots: u32,
    pub hits: u32,
    /// Detector reasons raised this session, in the order they were raised.
    pub flags: Vec<&'static str>,
}

pub struct Game {
    net: NetServer,
    relay: Option<Relay>,
    relay_static: Option<RelayStatic>,
    edge: EdgeKey,
    monitor: Monitor,
    tallies: BTreeMap<PlayerId, Tally>,
    log: Option<Box<dyn Write + Send>>,
    bots: Vec<BotThread>,
}

fn spawns() -> Vec<Vec2> {
    (0..SPAWNS)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / SPAWNS as f32;
            Vec2::new(SPAWN_RADIUS * a.cos(), SPAWN_RADIUS * a.sin())
        })
        .collect()
}

impl Game {
    /// Bind, start the relay (unless `direct`) and the bots. Players join
    /// from the first [`Game::tick`] on.
    pub fn start(cfg: &Config) -> io::Result<Self> {
        let server = Server::with_walls(spawns(), &ARENA_WALLS);
        let edge = EdgeKey::random();
        let (net, relay, relay_static) = if cfg.direct {
            (NetServer::bind(cfg.listen, server)?, None, None)
        } else {
            // The origin on loopback, at a port no one is told.
            let mut net = NetServer::bind((Ipv4Addr::LOCALHOST, 0), server)?;
            let link = LinkKey::random();
            let relay = Relay::spawn(cfg.listen, (Ipv4Addr::LOCALHOST, 0), net.local_addr()?, link, edge)?;
            net.behind_relay(relay.upstream_addr(), link);
            let me = RelayStatic::generate();
            relay.enable_noise(me.clone(), SESSION_TTL_SECS);
            (net, Some(relay), Some(me))
        };
        let to = relay.as_ref().map_or(net.local_addr()?, Relay::public_addr);
        let mut game = Self {
            net,
            relay,
            relay_static,
            edge,
            monitor: Monitor::standard(),
            tallies: BTreeMap::new(),
            log: None,
            bots: Vec::new(),
        };
        for (i, &kind) in cfg.bots.iter().enumerate() {
            let seed = cfg.seed.wrapping_add(i as u32).wrapping_mul(0x9E37_79B9) | 1;
            let keys = game.mint();
            game.bots.push(BotThread::spawn(i, kind, seed, to, keys)?);
        }
        Ok(game)
    }

    /// Write every telemetry record, one JSON object per line, as it is
    /// streamed: the real telemetry the detector's lines are set from.
    pub fn log_to(&mut self, w: impl Write + Send + 'static) {
        self.log = Some(Box::new(w));
    }

    /// The address clients are told.
    pub fn public_addr(&self) -> SocketAddr {
        self.relay.as_ref().map_or_else(|| self.net.local_addr().expect("bound"), Relay::public_addr)
    }

    /// The relay's static public key, which a client pins to run the Noise
    /// handshake. `None` when `direct`.
    pub fn relay_public(&self) -> Option<[u8; 32]> {
        self.relay_static.as_ref().map(RelayStatic::public)
    }

    /// Keys for one client, as the game's backend would issue them after a
    /// login. `None` when `direct`: there is nothing to seal under.
    pub fn mint(&self) -> Option<ClientKeys> {
        self.relay.as_ref().map(|_| mint_connect(&self.edge, unix_now() + SESSION_TTL_SECS))
    }

    /// The tick the server is on.
    pub fn tick_number(&self) -> u32 {
        self.net.tick()
    }

    /// One tick against the clock: snapshots out, inputs in until the
    /// deadline, the world moved, and every record written streamed to the
    /// monitor (and the log). The alerts this tick raised.
    pub fn tick(&mut self) -> io::Result<Vec<Alert>> {
        self.net.run_tick()?;
        let mut alerts = Vec::new();
        for r in self.net.drain_telemetry() {
            if let Some(w) = &mut self.log {
                serde_json::to_writer(&mut *w, &r)?;
                w.write_all(b"\n")?;
            }
            let raised = self.monitor.observe(&r);
            self.count(&r, &raised);
            alerts.extend(raised);
        }
        Ok(alerts)
    }

    fn count(&mut self, r: &Record, raised: &[Alert]) {
        if r.outcome == Outcome::Left {
            self.tallies.remove(&r.player);
            return;
        }
        let t = self.tallies.entry(r.player).or_default();
        match r.outcome {
            Outcome::Accepted { .. } => t.accepted += 1,
            Outcome::Rejected { .. } => t.rejected += 1,
            Outcome::Shot { hit, .. } => {
                t.shots += 1;
                t.hits += u32::from(hit);
            }
            Outcome::Glimpse { .. } | Outcome::Left => {}
        }
        t.flags.extend(raised.iter().map(|a| a.flag.reason.label()));
    }

    /// Every live session's tally, by player id.
    pub fn tallies(&self) -> &BTreeMap<PlayerId, Tally> {
        &self.tallies
    }

    /// Each bot and its player id once admitted.
    pub fn bots(&self) -> impl Iterator<Item = (BotKind, Option<PlayerId>)> + '_ {
        self.bots.iter().map(|b| (b.kind, b.player_id()))
    }

    pub fn server(&self) -> &Server {
        self.net.server()
    }
}

/// Unix seconds now, for a session's expiry.
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `bytes` as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
