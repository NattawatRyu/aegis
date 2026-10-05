//! Aegis harness — the lab bench.
//!
//! Puts every bot in one arena, runs N ticks against the real guards and the
//! real sim, and reports per bot what was accepted, flagged and rejected. It is
//! how "the speedhack is blocked" becomes a number instead of a claim.
//!
//! Everything goes through the wire: each bot's messages are encoded to bytes
//! and handed to [`Server::receive`] with the bot's source address, exactly as
//! the UDP loop does. Two transports carry those bytes:
//!   - [`run`] — in-process, deterministic (no sockets, no clock, no RNG): two
//!     runs of the same scenario produce byte-identical telemetry.
//!   - [`run_udp`] — real sockets over loopback, one IP per bot, driven in
//!     lockstep (each tick reads exactly the datagrams that were sent). Its
//!     telemetry must be byte-identical to [`run`]'s: the in-process run is
//!     the oracle for the network one.
//!
//! All server behaviour lives in [`aegis_server`]; this crate only drives bots
//! against it and keeps the lab's measurements (kills, largest step).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use aegis_client_sdk::{
    aimbot::AimbotBot,
    badversion::BadVersionBot,
    burst::BurstBot,
    camper::CamperBot,
    direct::DirectBot,
    esp::EspBot,
    flood::FloodBot,
    garbage::GarbageBot,
    honest::HonestBot,
    humanized::HumanizedAimbot,
    joinflood::JoinFloodBot,
    nan::NanBot,
    reflect::{ReflectBot, BYSTANDER},
    replay::ReplayBot,
    rusher::RusherBot,
    speedhack::SpeedhackBot,
    spoof::SpoofBot,
    zeroflood::ZeroFloodBot,
    Bot, BotCtx,
};
use aegis_detector::{Alert, Monitor, PlayerStats};
use aegis_protocol::{
    decode, encode, frame, split_frame, ClientMsg, LinkKey, PlayerId, PlayerState, ServerMsg, Vec2, NO_TOKEN,
};
use aegis_relay::{Clock, Relay, RelayStats};
use aegis_server::net::MAX_DATAGRAM;
use aegis_server::{NetServer, NetStats, Reply, Server, Session, Sim, TickOutcome, ARENA_WALLS};
use aegis_telemetry::{Telemetry, Totals};

/// Bots spawn evenly on a circle this far from the center — inside every
/// bot's shot range, so the aimbot always has targets.
pub const SPAWN_RADIUS: f32 = 20.0;

/// How long the UDP run waits for a datagram it knows was sent before calling
/// it lost. Loopback does not drop under this load; if it ever does, the run
/// fails loudly instead of comparing a short tick.
pub const UDP_WAIT: Duration = Duration::from_secs(2);

pub struct Scenario {
    pub name: &'static str,
    pub ticks: u32,
    pub bots: Vec<Box<dyn Bot>>,
}

impl Scenario {
    /// One of every bot, 10 seconds at 30Hz.
    pub fn standard() -> Self {
        Self {
            name: "standard",
            ticks: 300,
            bots: vec![
                Box::new(HonestBot::new()),
                Box::new(SpeedhackBot::new()),
                Box::new(FloodBot::default()),
                Box::new(ReplayBot::new()),
                Box::new(BadVersionBot::new()),
                Box::new(GarbageBot::new()),
                Box::new(NanBot::new()),
                Box::new(AimbotBot::new()),
                Box::new(HumanizedAimbot::new()),
                Box::new(SpoofBot::new()),
                Box::new(JoinFloodBot::new()),
                Box::new(ReflectBot::new()),
                Box::new(EspBot::new()),
                Box::new(ZeroFloodBot::new()),
                Box::new(BurstBot::new()),
            ],
        }
    }

    /// For each bot, the slot whose address its per-tick datagrams leave
    /// from: its own, or its victim's if it forges one. The bystander — an
    /// address nobody plays from — is the slot one past the last bot.
    fn routes(&self) -> Vec<usize> {
        self.bots
            .iter()
            .enumerate()
            .map(|(i, b)| match b.impersonates() {
                None => i,
                Some(BYSTANDER) => self.bystander(),
                Some(v) => {
                    self.bots.iter().position(|o| o.name() == v).unwrap_or_else(|| panic!("no victim named {v}"))
                }
            })
            .collect()
    }

    fn bystander(&self) -> usize {
        self.bots.len()
    }

    /// A lobby of `LOBBY_SIZE` honest walkers, each with its own aim seed
    /// derived from `seed` — the world the culling measurements replay.
    pub fn honest_lobby(seed: u32) -> Self {
        Self {
            name: "honest_lobby",
            ticks: 300,
            bots: (0..LOBBY_SIZE)
                .map(|k| {
                    Box::new(HonestBot::with_seed(seed.wrapping_mul(LOBBY_SIZE).wrapping_add(k + 1))) as Box<dyn Bot>
                })
                .collect(),
        }
    }

    /// A full arena of `CROWD_SIZE` honest players — the population the
    /// detector's false-positive bound is measured on. Each has its own aim
    /// seed derived from `seed`; per four: a walker, a camper, two rushers.
    ///
    /// Hit rate depends on more than aim. A rusher closing on a player who
    /// holds still has the easiest honest shots there are, and how often that
    /// happens grows with how full the arena is: one rusher among 3 campers
    /// peaked at ~0.4, among 14 at ~0.77. A sweep of 4-player walker lobbies
    /// (the old population) never saw either, and the esp bot — culled, an
    /// honest rusher — tripped the accuracy line in the 15-player standard
    /// scenario. Nobody here cheats, so any flag raised in it is a false
    /// positive.
    pub fn honest_crowd(seed: u32) -> Self {
        Self {
            name: "honest_crowd",
            ticks: 300,
            bots: (0..CROWD_SIZE)
                .map(|k| {
                    let s = seed.wrapping_mul(CROWD_SIZE).wrapping_add(k + 1);
                    match k % 4 {
                        0 => Box::new(HonestBot::with_seed(s)) as Box<dyn Bot>,
                        2 => Box::new(CamperBot::with_seed(s)),
                        _ => Box::new(RusherBot::with_seed(s)),
                    }
                })
                .collect(),
        }
    }
}

/// Players per [`Scenario::honest_crowd`]: at least the standard scenario's
/// head count, so the honest population is never measured in an emptier
/// arena than the cheaters are.
pub const CROWD_SIZE: u32 = 16;

impl Scenario {
    /// An honest player and one that sends to the origin's own address. Run
    /// with and without a relay: the before/after of hiding the origin.
    pub fn relay_probe() -> Self {
        Self { name: "relay_probe", ticks: 60, bots: vec![Box::new(HonestBot::new()), Box::new(DirectBot::new())] }
    }
}

/// Players per [`Scenario::honest_lobby`].
pub const LOBBY_SIZE: u32 = 4;

#[derive(Debug)]
pub struct BotReport {
    pub name: &'static str,
    /// The id the server issued — `None` if its join was never legal.
    pub id: Option<PlayerId>,
    pub totals: Totals,
    /// Shots that count as aim evidence — the `Shot` records in telemetry, so
    /// the detector and this report count the same thing. Point-blank shots
    /// still resolve (and can kill) but are not in here.
    pub shots: u32,
    pub hits: u32,
    pub kills: u32,
    /// Largest distance moved in a single tick. The sim's movement authority
    /// holds iff this never exceeds `MOVE_SPEED` for any bot.
    pub max_step: f32,
    /// Players this bot was sent, summed over ticks, that were behind a wall
    /// from where it stood — what ESP would draw. Snapshot culling holds iff
    /// this is 0 for every bot.
    pub hidden: u32,
    /// Players behind a wall from it in the whole world, summed over the same
    /// ticks — what an uncull'd snapshot would have leaked. `hidden` of
    /// `walled` is how much of it got out.
    pub walled: u32,
    /// What the online detector raised on this player, fed the run's telemetry
    /// one record at a time in stream order (what a live server would feed
    /// it), each with the tick it fired.
    pub alerts: Vec<Alert>,
}

pub struct Report {
    pub scenario: &'static str,
    pub ticks: u32,
    pub bots: Vec<BotReport>,
    pub telemetry: Telemetry,
    /// Datagrams dropped with no player to pin them on (source-rate drops,
    /// anything from an address that never joined).
    pub net: NetStats,
    /// Bytes sent in the bystander's name (forged), and bytes the server sent
    /// to the bystander. rx / tx is the server's amplification factor.
    pub bystander: Traffic,
    /// What the relay forwarded and dropped at the edge, for a relay run.
    pub relay: Option<RelayStats>,
    /// The whole world at the start of every tick (index tick - 1) — what
    /// [`cull_sweep`] replays. Kept by the in-process run only.
    pub frames: Vec<Vec<PlayerState>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Traffic {
    pub tx: u64,
    pub rx: u64,
}

impl BotReport {
    pub fn joined(&self) -> bool {
        self.id.is_some()
    }

    pub fn accuracy(&self) -> f32 {
        if self.shots == 0 {
            0.0
        } else {
            self.hits as f32 / self.shots as f32
        }
    }
}

impl Report {
    pub fn bot(&self, name: &str) -> &BotReport {
        self.bots.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no bot named {name}"))
    }
}

fn spawns(n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            Vec2::new(SPAWN_RADIUS * a.cos(), SPAWN_RADIUS * a.sin())
        })
        .collect()
}

/// Bot `i` sends from its own IP, as on a real network: the source-rate guard
/// budgets per IP. The server takes 127.0.0.1. Both runs use these IPs, so
/// both admit the same players in the same order.
pub fn bot_ip(i: usize) -> Ipv4Addr {
    Ipv4Addr::new(127, 0, 0, u8::try_from(i + 2).expect("at most 254 bots"))
}

/// One honest player as the detector saw them.
#[derive(Debug)]
pub struct HonestPlayer {
    /// How it plays: the bot's name (`honest` walks, `rusher` closes in).
    pub style: &'static str,
    /// Whole-run stats — what offline v0 judges.
    pub life: PlayerStats,
    /// The highest each signal reached at any record, over every view the
    /// online monitor judged (running lifetime and window, each only once it
    /// had its detector's minimum samples). A line below this would have
    /// fired on an honest player somewhere mid-run.
    pub peak: Peak,
    /// What the online monitor raised.
    pub alerts: Vec<Alert>,
}

/// Per-signal maxima; see [`HonestPlayer::peak`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Peak {
    pub accuracy: f32,
    pub aim_exact: f32,
    pub anomaly_rate: f32,
}

impl Peak {
    fn raise(&mut self, s: &PlayerStats) {
        use aegis_detector::detectors::{accuracy, aim_exact, anomaly_rate};
        if s.shots >= accuracy::MIN_SHOTS {
            self.accuracy = self.accuracy.max(s.hits as f32 / s.shots as f32);
        }
        if s.shots >= aim_exact::MIN_SHOTS {
            self.aim_exact = self.aim_exact.max(s.exact as f32 / s.shots as f32);
        }
        if s.accepted >= anomaly_rate::MIN_INPUTS {
            self.anomaly_rate = self.anomaly_rate.max(s.anomalies as f32 / s.accepted as f32);
        }
    }
}

/// Every player across `crowds` honest crowds (seeds 0..crowds) — the honest
/// population the thresholds are measured against.
pub fn honest_sweep(crowds: u32) -> Vec<HonestPlayer> {
    (0..crowds)
        .flat_map(|seed| {
            let r = run(Scenario::honest_crowd(seed));
            let style = |p: PlayerId| r.bots.iter().find(|b| b.id == Some(p)).expect("a bot per player").name;
            let tel = &r.telemetry;
            let mut m = Monitor::standard();
            let mut peaks: BTreeMap<PlayerId, Peak> = BTreeMap::new();
            let mut alerts = Vec::new();
            for r in tel.records() {
                alerts.extend(m.observe(r));
                let peak = peaks.entry(r.player).or_default();
                m.stats(r.player).into_iter().chain(m.window(r.player)).for_each(|s| peak.raise(s));
            }
            aegis_detector::stats(tel.records())
                .into_values()
                .map(|life| HonestPlayer {
                    style: style(life.player),
                    peak: peaks[&life.player],
                    alerts: alerts.iter().filter(|a| a.flag.player == life.player).cloned().collect(),
                    life,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What [`cull_sweep`] counted. Every count is over (viewer, enemy, tick)
/// triples; `margin` indexes are 0..=max_lag, `lag` indexes likewise.
#[derive(Debug)]
pub struct CullSweep {
    /// Triples where the enemy was in sight.
    pub visible: u64,
    /// Triples where the enemy was behind a wall.
    pub walled: u64,
    /// `leaked[margin]`: walled triples a view with that margin sent anyway.
    pub leaked: Vec<u64>,
    /// `late[margin][lag]`: visible triples missing from the view sent `lag`
    /// ticks earlier with that margin — the pop-in a client that far behind
    /// sees. `late[l][l]` is the margin's own sampling miss.
    pub late: Vec<Vec<u64>>,
}

/// Replay `lobbies` honest lobbies (seeds 0..lobbies) and measure, for each
/// margin 0..=`max_lag` ticks, what culling with it leaks and how late it
/// shows an enemy to a client `lag` ticks behind. The measurement
/// [`aegis_server::MAX_MARGIN_TICKS`] is set from.
pub fn cull_sweep(lobbies: u32, max_lag: u32) -> CullSweep {
    let m = max_lag as usize;
    let mut s = CullSweep { visible: 0, walled: 0, leaked: vec![0; m + 1], late: vec![vec![0; m + 1]; m + 1] };
    for seed in 0..lobbies {
        let (frames, worlds) = replay(seed);
        // sent[margin][tick] = (viewer, ids it was sent)
        type Views = Vec<(PlayerId, Vec<PlayerId>)>;
        let sent: Vec<Vec<Views>> = (0..=max_lag)
            .map(|k| {
                worlds
                    .iter()
                    .zip(&frames)
                    .map(|(w, f)| {
                        f.iter().map(|v| (v.id, w.view_within(v.id, k).iter().map(|p| p.id).collect())).collect()
                    })
                    .collect()
            })
            .collect();
        let was_sent = |k: usize, t: usize, v: PlayerId, p: PlayerId| {
            sent[k][t].iter().any(|(id, ids)| *id == v && ids.contains(&p))
        };
        // from tick m on, so every lag has a snapshot to look back at
        for t in m..frames.len() {
            for v in &frames[t] {
                for p in frames[t].iter().filter(|p| p.id != v.id) {
                    if worlds[t].sees(v.pos, p.pos) {
                        s.visible += 1;
                        for k in 0..=m {
                            for l in 0..=m {
                                s.late[k][l] += u64::from(!was_sent(k, t - l, v.id, p.id));
                            }
                        }
                    } else {
                        s.walled += 1;
                        for k in 0..=m {
                            s.leaked[k] += u64::from(was_sent(k, t, v.id, p.id));
                        }
                    }
                }
            }
        }
    }
    s
}

/// Honest lobby `seed`'s frames, and each rebuilt as a world in the arena.
fn replay(seed: u32) -> (Vec<Vec<PlayerState>>, Vec<Sim>) {
    let frames = run(Scenario::honest_lobby(seed)).frames;
    let worlds = frames
        .iter()
        .map(|f| {
            let mut w = Sim::with_walls(&ARENA_WALLS);
            f.iter().for_each(|p| w.spawn(p.id, p.pos));
            w
        })
        .collect();
    (frames, worlds)
}

/// What a margin of `ticks` would leak if players moved `step` a tick
/// instead of `MOVE_SPEED`: for each step, for margin 0..=`max_margin`, the
/// walled (viewer, enemy, tick) triples it sends anyway — and, first, how
/// many walled triples there were. Positions are the honest lobbies' real
/// ones; only the margin's reach is rescaled. Pop-in is not measured here:
/// a margin of at least the lag reaches everywhere a player can, so it is
/// late only by its sampling miss ([`cull_sweep`]'s `late[l][l]`).
pub fn leak_by_step(lobbies: u32, steps: &[f32], max_margin: u32) -> (u64, Vec<Vec<u64>>) {
    let mut walled = 0;
    let mut leaked = vec![vec![0u64; max_margin as usize + 1]; steps.len()];
    for seed in 0..lobbies {
        let (frames, worlds) = replay(seed);
        for (f, w) in frames.iter().zip(&worlds) {
            for v in f {
                for p in f.iter().filter(|p| p.id != v.id && !w.sees(v.pos, p.pos)) {
                    walled += 1;
                    for (si, &step) in steps.iter().enumerate() {
                        for k in 0..=max_margin {
                            leaked[si][k as usize] += u64::from(w.sees_within_step(v.pos, p.pos, k, step));
                        }
                    }
                }
            }
        }
    }
    (walled, leaked)
}

/// What the harness tracks per bot while the scenario runs, whichever
/// transport carries the bytes. Indexed by bot slot.
struct Lab {
    sessions: Vec<Option<Session>>,
    kills: Vec<u32>,
    max_step: Vec<f32>,
    hidden: Vec<u32>,
    walled: Vec<u32>,
    bystander: Traffic,
}

impl Lab {
    fn new(n: usize) -> Self {
        Self {
            sessions: vec![None; n],
            kills: vec![0; n],
            max_step: vec![0.0; n],
            hidden: vec![0; n],
            walled: vec![0; n],
            bystander: Traffic::default(),
        }
    }

    /// Count the players in what bot `i` was sent that it could not see from
    /// where it stands — what a wallhack would draw. Culling makes it 0.
    /// Beside it, the same count over the whole world: what a full snapshot
    /// (every client's before D4) would have handed it.
    fn count_hidden(&mut self, i: usize, sim: &Sim, seen: &[PlayerState]) {
        let Some(me) = self.id(i).and_then(|p| seen.iter().find(|s| s.id == p)) else { return };
        let behind =
            |ps: &[PlayerState]| ps.iter().filter(|p| p.id != me.id && !sim.sees(me.pos, p.pos)).count() as u32;
        self.hidden[i] += behind(seen);
        self.walled[i] += behind(&sim.snapshot());
    }

    fn id(&self, i: usize) -> Option<PlayerId> {
        self.sessions[i].map(|s| s.player_id)
    }

    fn ctx<'a>(&self, i: usize, tick: u32, snapshot: &'a [PlayerState]) -> BotCtx<'a> {
        // An unadmitted bot has no id (0 is never issued) and no token.
        let s = self.sessions[i];
        BotCtx { tick, my_id: s.map_or(0, |s| s.player_id), token: s.map_or(NO_TOKEN, |s| s.token), snapshot }
    }

    fn slot(&self, p: PlayerId) -> usize {
        (0..self.sessions.len()).position(|i| self.id(i) == Some(p)).expect("outcome for a player no bot owns")
    }

    fn measure(&mut self, out: TickOutcome) {
        for p in out.kills {
            let s = self.slot(p);
            self.kills[s] += 1;
        }
        for (p, d) in out.steps {
            let s = self.slot(p);
            self.max_step[s] = self.max_step[s].max(d);
        }
    }

    fn report(self, sc: &Scenario, tel: Telemetry, net: NetStats) -> Report {
        let alerts = Monitor::standard().run(tel.records());
        let bots = sc
            .bots
            .iter()
            .enumerate()
            .map(|(i, bot)| {
                let id = self.id(i);
                let totals = id.map(|p| tel.per_player(p)).unwrap_or_default();
                let alerts = alerts.iter().filter(|a| Some(a.flag.player) == id).cloned().collect();
                BotReport {
                    name: bot.name(),
                    id,
                    shots: totals.shots,
                    hits: totals.hits,
                    totals,
                    kills: self.kills[i],
                    max_step: self.max_step[i],
                    hidden: self.hidden[i],
                    walled: self.walled[i],
                    alerts,
                }
            })
            .collect();
        Report {
            scenario: sc.name,
            ticks: sc.ticks,
            bots,
            telemetry: tel,
            net,
            bystander: self.bystander,
            relay: None,
            frames: Vec::new(),
        }
    }
}

/// Run a scenario in-process: the server is called directly with each bot's
/// address. Deterministic.
pub fn run(mut sc: Scenario) -> Report {
    let n = sc.bots.len();
    let route = sc.routes();
    let mut server = Server::with_walls(spawns(n), &ARENA_WALLS);
    let mut lab = Lab::new(n);

    // Tick 0: the handshake, over the wire like everything else.
    let mut retry = Vec::new();
    for (i, bot) in sc.bots.iter().enumerate() {
        let d = frame(NO_TOKEN, &bot.join());
        match server.receive(0, mem_addr(i, 0), &d) {
            Some(Reply::Joined(s)) => lab.sessions[i] = Some(s),
            Some(Reply::Challenge(c)) => retry.extend(with_cookie(&d, c).map(|r| (i, 0, r))),
            None => {}
        }
    }
    answer_challenges(&mut server, &mut lab, 0, retry);

    let bystander = mem_addr(sc.bystander(), 0);
    let mut frames = Vec::with_capacity(sc.ticks as usize);
    for tick in 1..=sc.ticks {
        server.begin_tick(tick);
        frames.push(server.sim().snapshot());
        // Each admitted address is sent its player's view, all taken before
        // any input this tick (as the UDP run sends them all first).
        let views: Vec<Vec<PlayerState>> = (0..n)
            .map(|i| server.player_id(mem_addr(i, 0)).map(|p| server.sim().view(p)).unwrap_or_default())
            .collect();
        if let Some(p) = server.player_id(bystander) {
            lab.bystander.rx += encode(&ServerMsg::Snapshot { tick, players: server.sim().view(p) }).len() as u64;
        }
        let mut retry = Vec::new();
        for (i, bot) in sc.bots.iter_mut().enumerate() {
            let seen = &views[i];
            lab.count_hidden(i, server.sim(), seen);
            for (k, d) in bot.routed(&lab.ctx(i, tick, seen)) {
                // A reply goes to the address the datagram came from. Only
                // port 0's Joined is read back (the UDP run reads only that
                // one), and only a bot's own address gets to answer a
                // challenge — a forger never receives it.
                let from = mem_addr(route[i], k);
                if from == bystander {
                    lab.bystander.tx += d.len() as u64;
                }
                let reply = server.receive(tick, from, &d);
                if let (Some(r), true) = (reply, from == bystander) {
                    lab.bystander.rx += encode(&r.to_msg(tick)).len() as u64;
                }
                match reply {
                    Some(Reply::Joined(s)) if k == 0 && route[i] < n => lab.sessions[route[i]] = Some(s),
                    Some(Reply::Challenge(c)) if route[i] == i => retry.extend(with_cookie(&d, c).map(|r| (i, k, r))),
                    _ => {}
                }
            }
        }
        answer_challenges(&mut server, &mut lab, tick, retry);
        lab.measure(server.end_tick(tick));
    }

    let (tel, net) = server.into_parts();
    Report { frames, ..lab.report(&sc, tel, net) }
}

/// Run a scenario over real UDP on loopback: a [`NetServer`] and one socket
/// per bot. Lockstep, not real-time — each tick the server reads exactly as
/// many datagrams as the bots sent — so the result does not depend on timing.
/// What a bot sees (its id, the snapshot) comes off the wire.
pub fn run_udp(sc: Scenario) -> io::Result<Report> {
    run_net(sc, false)
}

/// The same, with the server as an origin behind an [`aegis_relay::Relay`]:
/// bots are given the relay's address, and every datagram a bot reads must
/// come from it. A bot that [`Bot::bypasses_relay`] sends to the origin's
/// address instead.
pub fn run_relay(sc: Scenario) -> io::Result<Report> {
    run_net(sc, true)
}

fn run_net(mut sc: Scenario, relayed: bool) -> io::Result<Report> {
    let n = sc.bots.len();
    let mut net = NetServer::bind((Ipv4Addr::LOCALHOST, 0), Server::with_walls(spawns(n), &ARENA_WALLS))?;
    let origin = net.local_addr()?;
    let relay = if relayed {
        // A fresh link key per run, as a deployment would provision one.
        let key = LinkKey::random();
        // Lockstep budget windows: one per origin tick, moved below.
        let r = Relay::spawn_with((Ipv4Addr::LOCALHOST, 0), (Ipv4Addr::LOCALHOST, 0), origin, key, Clock::lockstep())?;
        net.behind_relay(r.upstream_addr(), key);
        Some(r)
    } else {
        None
    };
    // The one server address bots are told; the only one they hear from.
    let to = relay.as_ref().map_or(origin, Relay::public_addr);
    let dest: Vec<SocketAddr> = sc.bots.iter().map(|b| if b.bypasses_relay() { origin } else { to }).collect();
    // Behind a relay, which bots it answers: the relay, not the origin,
    // challenges their Joins.
    let edge: Option<Vec<bool>> = relayed.then(|| dest.iter().map(|&d| d != origin).collect());
    // socks[i][k]: bot slot i, source port index k, all on the bot's IP.
    let mut socks = sc
        .bots
        .iter()
        .enumerate()
        .map(|(i, bot)| {
            (0..bot.sources())
                .map(|_| {
                    let s = UdpSocket::bind((bot_ip(i), 0))?;
                    s.set_read_timeout(Some(UDP_WAIT))?;
                    Ok(s)
                })
                .collect::<io::Result<Vec<_>>>()
        })
        .collect::<io::Result<Vec<_>>>()?;
    let route = sc.routes();
    let mut lab = Lab::new(n);
    // Which bot socket an address belongs to: where a reply to it lands.
    let mut owner = HashMap::new();
    for (i, ports) in socks.iter().enumerate() {
        for (k, s) in ports.iter().enumerate() {
            owner.insert(s.local_addr()?, (i, k as u16));
        }
    }
    // The bystander: one more slot, after the map, so nothing it is sent is
    // ever read as a bot's or answered. It only counts bytes.
    let bystander = UdpSocket::bind((bot_ip(sc.bystander()), 0))?;
    bystander.set_nonblocking(true)?;
    socks.push(vec![bystander]);
    net.log_replies();

    let rl = relay.as_ref();
    // How many of a batch went to the relay and how many straight on.
    let split = |sent: &[(usize, usize, u16, Vec<u8>)]| {
        let direct = sent.iter().filter(|(i, ..)| relayed && dest[*i] == origin).count();
        (sent.len() - direct, direct)
    };

    // Tick 0: the handshake. Each sent datagram is (bot, slot, port, bytes).
    let before = relay_stats(rl);
    let mut sent = Vec::new();
    for (i, bot) in sc.bots.iter().enumerate() {
        let d = frame(NO_TOKEN, &bot.join());
        socks[i][0].send_to(&d, dest[i])?;
        sent.push((i, i, 0, d));
    }
    let (via, direct) = split(&sent);
    deliver(&mut net, rl, before, via, direct)?;
    let retry = udp_challenges(&mut net, edge.as_deref(), &socks, &owner, &sent, to)?;
    answer_udp(&mut net, rl, &socks, to, retry)?;

    for tick in 1..=sc.ticks {
        net.begin_tick()?;
        if let Some(r) = rl {
            r.next_window();
        }
        let before = relay_stats(rl);
        let mut sent = Vec::new();
        for (i, bot) in sc.bots.iter_mut().enumerate() {
            // The server's word on who is admitted decides only whether to
            // wait for a snapshot; what is in it is read off the socket.
            let snapshot = match net.server().player_id(socks[i][0].local_addr()?) {
                Some(_) => read_snapshot(&socks[i][0], to, tick, &mut lab.sessions[i])?,
                None => Vec::new(),
            };
            lab.count_hidden(i, net.server().sim(), &snapshot);
            for (k, d) in bot.routed(&lab.ctx(i, tick, &snapshot)) {
                if route[i] == n {
                    lab.bystander.tx += d.len() as u64;
                }
                socks[route[i]][k as usize].send_to(&d, dest[i])?;
                sent.push((i, route[i], k, d));
            }
        }
        let (via, direct) = split(&sent);
        deliver(&mut net, rl, before, via, direct)?;
        let retry = udp_challenges(&mut net, edge.as_deref(), &socks, &owner, &sent, to)?;
        answer_udp(&mut net, rl, &socks, to, retry)?;
        drain_extra_ports(&socks)?;
        lab.bystander.rx += drain_bytes(&socks[n][0], Duration::ZERO)?;
        lab.measure(net.end_tick());
    }
    lab.bystander.rx += drain_bytes(&socks[n][0], Duration::from_millis(100))?;

    let (tel, stats) = net.into_server().into_parts();
    let mut report = lab.report(&sc, tel, stats);
    report.relay = relay.as_ref().map(Relay::stats);
    Ok(report)
}

/// In-process address of bot slot `i`, source port index `k`.
fn mem_addr(i: usize, k: u16) -> SocketAddr {
    SocketAddr::from((bot_ip(i), 40000 + k))
}

/// The second phase of an in-process tick: after every bot has sent, each
/// challenge a bot received at its own address is answered, in the order
/// the challenges went out (the UDP run answers in the same order).
fn answer_challenges(server: &mut Server, lab: &mut Lab, tick: u32, retry: Vec<(usize, u16, Vec<u8>)>) {
    for (i, k, d) in retry {
        if let Some(Reply::Joined(s)) = server.receive(tick, mem_addr(i, k), &d) {
            if k == 0 {
                lab.sessions[i] = Some(s);
            }
        }
    }
}

/// What a client does when challenged: the same Join, sent again with the
/// cookie. `None` if `datagram` is not a cookieless Join.
fn with_cookie(datagram: &[u8], cookie: u64) -> Option<Vec<u8>> {
    let (_, body) = split_frame(datagram)?;
    match decode::<ClientMsg>(body).ok()? {
        ClientMsg::Join { name, protocol, cookie: None } => {
            Some(frame(NO_TOKEN, &ClientMsg::Join { name, protocol, cookie: Some(cookie) }))
        }
        _ => None,
    }
}

/// Feed the server what was just sent: `direct` datagrams that went straight
/// to it, and `via` that went to the relay — of which only the ones the relay
/// forwarded will ever arrive. First waits until the relay has accounted for
/// every one of the `via` (forwarded or dropped, by its counters since
/// `before`), so the count is exact, never a guess against the clock.
fn deliver(
    net: &mut NetServer,
    relay: Option<&Relay>,
    before: RelayStats,
    via: usize,
    direct: usize,
) -> io::Result<()> {
    let forwarded = match relay {
        None => via,
        Some(r) => {
            let start = Instant::now();
            loop {
                let s = r.stats();
                let seen = edge_seen(&s) - edge_seen(&before);
                if seen >= via as u64 {
                    break (s.up - before.up) as usize;
                }
                if start.elapsed() > UDP_WAIT {
                    let msg = format!("tick {}: the relay saw {seen} of {via} datagrams", net.tick());
                    return Err(io::Error::new(io::ErrorKind::TimedOut, msg));
                }
                std::thread::yield_now();
            }
        }
    };
    pump(net, forwarded + direct)
}

/// Client datagrams the relay has accounted for: forwarded or dropped.
pub fn edge_seen(s: &RelayStats) -> u64 {
    s.up + s.challenged + edge_dropped(s)
}

/// Client datagrams the relay dropped at the edge.
pub fn edge_dropped(s: &RelayStats) -> u64 {
    s.short + s.bad_token + s.join_rate + s.bad_cookie + s.oversize
}

/// Relay counters now, or zeros without a relay.
fn relay_stats(relay: Option<&Relay>) -> RelayStats {
    relay.map(Relay::stats).unwrap_or_default()
}

/// Feed the server exactly `expected` datagrams, or fail.
fn pump(net: &mut NetServer, expected: usize) -> io::Result<()> {
    for got in 0..expected {
        if !net.recv_one(UDP_WAIT)? {
            let msg = format!("tick {}: lost {} of {} datagrams", net.tick(), expected - got, expected);
            return Err(io::Error::new(io::ErrorKind::TimedOut, msg));
        }
    }
    Ok(())
}

/// The UDP side of a tick's second phase, step one: every challenge the
/// server sent, in send order, read off the socket it went to — and kept for
/// an answer only if that socket's own bot sent the cookieless Join (a
/// forger's victim would not answer a challenge it never asked for). Same
/// rule and order as the in-process run.
///
/// Behind a relay (`edge`: per bot, whether it sends via the relay) the relay
/// does the challenging, so the origin must have sent none — one would mean
/// an unproven Join crossed — and each cookieless Join a bot sent from its
/// own address via the relay is answered, in send order, with the cookie
/// read off that socket.
#[allow(clippy::type_complexity)]
fn udp_challenges(
    net: &mut NetServer,
    edge: Option<&[bool]>,
    socks: &[Vec<UdpSocket>],
    owner: &HashMap<SocketAddr, (usize, u16)>,
    sent: &[(usize, usize, u16, Vec<u8>)],
    server: SocketAddr,
) -> io::Result<Vec<(usize, u16, Vec<u8>)>> {
    let mut retry = Vec::new();
    if let Some(via_relay) = edge {
        if net.take_replies().iter().any(|(_, m)| matches!(m, ServerMsg::Challenge { .. })) {
            let msg =
                format!("tick {}: the origin challenged a join, so an unproven join crossed the relay", net.tick());
            return Err(io::Error::other(msg));
        }
        for (i, slot, k, d) in sent {
            if i != slot || !via_relay[*i] || with_cookie(d, 0).is_none() {
                continue;
            }
            let cookie = read_challenge(&socks[*slot][*k as usize], server)?;
            retry.extend(with_cookie(d, cookie).map(|r| (*slot, *k, r)));
        }
        return Ok(retry);
    }
    for (dest, msg) in net.take_replies() {
        let ServerMsg::Challenge { cookie } = msg else { continue };
        let Some(&(slot, k)) = owner.get(&dest) else { continue };
        wait_for(&socks[slot][k as usize], server, &msg)?;
        let asked = sent.iter().find(|(i, s, p, _)| *i == slot && *s == slot && *p == k);
        if let Some(d) = asked.and_then(|(_, _, _, d)| with_cookie(d, cookie)) {
            retry.push((slot, k, d));
        }
    }
    Ok(retry)
}

/// Step two: send the answers and let the server read them. The Joined
/// replies are left on the sockets (port 0's is read with the next snapshot).
fn answer_udp(
    net: &mut NetServer,
    relay: Option<&Relay>,
    socks: &[Vec<UdpSocket>],
    to: SocketAddr,
    retry: Vec<(usize, u16, Vec<u8>)>,
) -> io::Result<()> {
    let before = relay_stats(relay);
    for (i, k, d) in &retry {
        socks[*i][*k as usize].send_to(d, to)?;
    }
    deliver(net, relay, before, retry.len(), 0)?;
    net.take_replies();
    Ok(())
}

/// One datagram off `sock`, which must have come from `server` — the only
/// server address a bot knows. Behind a relay, a datagram from anywhere else
/// would mean the origin's address reached a client.
fn recv_from_server(sock: &UdpSocket, server: SocketAddr, buf: &mut [u8]) -> io::Result<usize> {
    let (n, from) = sock.recv_from(buf)?;
    if from != server {
        let msg = format!("a bot heard from {from}, not the server address it was given ({server})");
        return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
    }
    Ok(n)
}

/// Read from `sock` until a challenge arrives, skipping whatever was queued
/// before it; its cookie.
fn read_challenge(sock: &UdpSocket, server: SocketAddr) -> io::Result<u64> {
    let mut buf = [0u8; MAX_DATAGRAM];
    loop {
        let n = recv_from_server(sock, server, &mut buf)?;
        if let Ok(ServerMsg::Challenge { cookie }) = decode::<ServerMsg>(&buf[..n]) {
            return Ok(cookie);
        }
    }
}

/// Read from `sock` until `want` arrives, skipping whatever was queued
/// before it (snapshots and Joined replies nobody reads on that port).
fn wait_for(sock: &UdpSocket, server: SocketAddr, want: &ServerMsg) -> io::Result<()> {
    let mut buf = [0u8; MAX_DATAGRAM];
    loop {
        let n = recv_from_server(sock, server, &mut buf)?;
        if decode::<ServerMsg>(&buf[..n]).ok().as_ref() == Some(want) {
            return Ok(());
        }
    }
}

/// Empty every extra source port (index >= 1). Nobody reads the snapshots
/// sent to them; left alone they fill the socket buffer and the next
/// challenge to that port would be dropped on arrival.
fn drain_extra_ports(socks: &[Vec<UdpSocket>]) -> io::Result<()> {
    let mut buf = [0u8; MAX_DATAGRAM];
    for s in socks.iter().flat_map(|ports| ports.iter().skip(1)) {
        s.set_nonblocking(true)?;
        loop {
            match s.recv(&mut buf) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
                Err(e) => return Err(e),
            }
        }
        s.set_nonblocking(false)?;
    }
    Ok(())
}

/// Bytes waiting on a non-blocking socket, read and counted (after `settle`,
/// for anything still in flight).
fn drain_bytes(sock: &UdpSocket, settle: Duration) -> io::Result<u64> {
    std::thread::sleep(settle);
    let mut buf = [0u8; MAX_DATAGRAM];
    let mut total = 0;
    loop {
        match sock.recv(&mut buf) {
            Ok(n) => total += n as u64,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(total),
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => return Err(e),
        }
    }
}

/// Read this tick's snapshot, taking any `Joined` that arrives first.
fn read_snapshot(
    sock: &UdpSocket,
    server: SocketAddr,
    tick: u32,
    session: &mut Option<Session>,
) -> io::Result<Vec<PlayerState>> {
    let mut buf = [0u8; MAX_DATAGRAM];
    loop {
        let n = recv_from_server(sock, server, &mut buf)?;
        match decode::<ServerMsg>(&buf[..n]) {
            Ok(ServerMsg::Joined { player_id, token, .. }) => *session = Some(Session { player_id, token }),
            Ok(ServerMsg::Snapshot { tick: t, players }) if t == tick => return Ok(players),
            other => {
                let msg = format!("tick {tick}: expected a snapshot, got {other:?}");
                return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_client_sdk::{burst, flood, garbage, spoof, zeroflood};
    use aegis_detector::detectors::{accuracy, aim_exact, anomaly_rate};
    use aegis_detector::monitor::WINDOW;
    use aegis_detector::{FlagReason, Suite};
    use aegis_server::guards::source_rate::MAX_PER_TICK;
    use aegis_server::net::NOT_RELAY;
    use aegis_server::sim::MOVE_SPEED;
    use aegis_server::RejectReason;

    fn standard() -> Report {
        run(Scenario::standard())
    }

    fn rejected(b: &BotReport, reason: &str) -> u32 {
        b.totals.rejected.get(reason).copied().unwrap_or(0)
    }

    fn jsonl(t: &Telemetry) -> Vec<u8> {
        let mut v = Vec::new();
        t.write_jsonl(&mut v).unwrap();
        v
    }

    #[test]
    fn honest_passes_clean() {
        let r = standard();
        let b = r.bot("honest");
        assert!(b.joined());
        assert_eq!(b.totals.accepted, r.ticks);
        assert_eq!(b.totals.anomalies, 0);
        assert_eq!(b.totals.total_rejected(), 0);
    }

    #[test]
    fn speedhack_is_flagged_every_tick_and_moves_no_faster() {
        let r = standard();
        let b = r.bot("speedhack");
        assert_eq!(b.totals.accepted, r.ticks);
        assert_eq!(b.totals.anomalies, r.ticks);
        assert!((b.max_step - MOVE_SPEED).abs() < 1e-4, "max_step {}", b.max_step);
    }

    #[test]
    fn nobody_ever_moves_faster_than_move_speed() {
        for b in &standard().bots {
            assert!(b.max_step <= MOVE_SPEED + 1e-4, "{} stepped {}", b.name, b.max_step);
        }
    }

    /// Two layers: source_rate drops everything past the cap undecoded (a
    /// counter, no record), then input_rate lets one of the rest through.
    #[test]
    fn flood_gets_exactly_one_input_per_tick_and_most_is_never_decoded() {
        let r = standard();
        let b = r.bot("flood");
        assert_eq!(b.totals.accepted, r.ticks);
        assert_eq!(rejected(b, "rate_exceeded"), (MAX_PER_TICK - 1) * r.ticks);
        assert_eq!(b.totals.total_rejected(), (MAX_PER_TICK - 1) * r.ticks);
    }

    /// Every source-rate drop is accounted for: the flood's excess over its
    /// own (player) budget, plus the spoof's and the zero-flood's excess over
    /// their IPs' unauthenticated budgets. Nothing else in the scenario is
    /// over a cap.
    #[test]
    fn source_rate_drops_are_exactly_the_three_floods() {
        let r = standard();
        let over = |per_tick: usize| u64::from((per_tick as u32 - MAX_PER_TICK) * r.ticks);
        assert_eq!(
            r.net.get("source_rate"),
            over(flood::DEFAULT_PER_TICK) + over(spoof::PER_TICK) + over(zeroflood::PER_TICK)
        );
    }

    /// The relay's token-0 budget is the origin's unauthenticated budget:
    /// if they differed, the relay would either drop what the origin admits
    /// or let through what it is there to stop.
    #[test]
    fn the_edge_budget_is_the_origins() {
        assert_eq!(aegis_relay::join_rate::MAX_PER_WINDOW, MAX_PER_TICK);
    }

    #[test]
    fn replay_gets_only_its_first_input() {
        let r = standard();
        let b = r.bot("replay");
        assert_eq!(b.totals.accepted, 1);
        assert_eq!(rejected(b, "replay"), r.ticks - 1);
    }

    /// Forged datagrams from the victim's address get nothing: the victim
    /// still gets every input in, its record stays clean (nothing to frame it
    /// with), it moves only as it chose — and the forgeries are counted.
    #[test]
    fn spoof_cannot_act_as_starve_or_frame_its_victim() {
        let r = standard();
        let v = r.bot(spoof::VICTIM);
        assert_eq!(v.totals.accepted, r.ticks);
        assert_eq!(v.totals.total_rejected(), 0, "victim's record: {:?}", v.totals.rejected);
        assert_eq!(v.totals.anomalies, 0);
        assert!(v.alerts.is_empty());
        let s = r.bot("spoof");
        assert!(s.joined());
        assert_eq!(s.totals, Totals::default()); // it never sent as itself
                                                 // Up to the victim IP's unauthenticated budget, each forgery is
                                                 // decoded and refused for its token; the rest never get that far.
        assert_eq!(r.net.get("bad_token"), u64::from(MAX_PER_TICK * r.ticks));
    }

    /// One IP, 255 ports, a legal Join from each: the server must never fill
    /// up. Every join it refuses is refused for the per-IP session cap.
    #[test]
    fn join_flood_never_fills_the_server() {
        let r = standard();
        assert_eq!(r.net.get("server_full"), 0, "net: {:?}", r.net);
        assert!(r.net.get("ip_sessions") > 0, "the cap never fired: {:?}", r.net);
    }

    /// Forged Joins in a bystander's name must not turn the server into an
    /// amplifier: an unproven address may get a challenge, never more bytes
    /// than were sent in its name — and is never admitted. The guessed
    /// cookie is refused every tick.
    #[test]
    fn reflect_cannot_use_the_server_as_an_amplifier() {
        let r = standard();
        let t = r.bystander;
        assert!(t.tx > 0);
        assert!(t.rx <= t.tx, "amplification {:.1}x: {t:?}", t.rx as f64 / t.tx as f64);
        assert_eq!(r.net.get("bad_cookie"), u64::from(r.ticks));
    }

    /// No legal join, no id: badversion leaves no per-player record at all,
    /// only counters — and costs no id that a real player could have had.
    #[test]
    fn badversion_never_gets_an_id_and_every_input_is_refused() {
        let r = standard();
        let b = r.bot("badversion");
        assert!(!b.joined());
        assert_eq!(b.totals, Totals::default());
        assert_eq!(r.net.get("bad_version"), 1);
        assert_eq!(r.net.get("not_joined"), u64::from(r.ticks));
        let mut ids: Vec<PlayerId> = r.bots.iter().filter_map(|b| b.id).collect();
        ids.sort();
        assert_eq!(ids, (1..=r.bots.len() as PlayerId - 1).collect::<Vec<_>>());
    }

    #[test]
    fn garbage_is_dropped_at_decode() {
        let r = standard();
        let b = r.bot("garbage");
        assert!(b.joined()); // the join was legal: only the packet guard stands in the way
        assert_eq!(b.totals.accepted, 0);
        assert_eq!(rejected(b, "malformed_packet"), garbage::SHAPES as u32 * r.ticks);
    }

    #[test]
    fn nan_is_stopped_by_sanity_before_the_sim() {
        let r = standard();
        let b = r.bot("nan");
        assert!(b.joined());
        assert_eq!(b.totals.accepted, 0);
        assert_eq!(rejected(b, "malformed_input"), r.ticks);
        assert_eq!(b.shots, 0); // a NaN aim never reached apply_shot
    }

    /// Coverage: every reject reason the server has is produced by some bot in
    /// the standard scenario — on a player's record or, for datagrams with no
    /// player, in the net counters. A guard no bot trips is a guard nobody has
    /// seen fire.
    #[test]
    fn every_reject_reason_fires() {
        let r = standard();
        let all = r.telemetry.totals();
        for reason in RejectReason::ALL {
            let l = reason.label();
            let n = u64::from(all.rejected.get(l).copied().unwrap_or(0)) + r.net.get(l);
            assert!(n > 0, "{l} never fired");
        }
    }

    /// The headline: the aimbot trips nothing, yet out-aims the honest player.
    /// Guards see a legal player; only its accuracy gives it away (pillar C).
    #[test]
    fn aimbot_passes_every_guard_and_out_aims_honest() {
        let r = standard();
        let (a, h) = (r.bot("aimbot"), r.bot("honest"));
        assert_eq!(a.totals.accepted, r.ticks);
        assert_eq!(a.totals.anomalies, 0);
        assert_eq!(a.totals.total_rejected(), 0);
        assert!(a.hits > 0 && h.shots > 0);
        assert!(a.accuracy() > 0.9, "aimbot accuracy {}", a.accuracy());
        assert!(h.accuracy() < 0.6, "honest accuracy {}", h.accuracy());
    }

    /// The reasons raised on a bot, sorted — the order they fired in is the
    /// alerts' business, not this list's.
    fn flagged(b: &BotReport) -> Vec<FlagReason> {
        let mut r: Vec<FlagReason> = b.alerts.iter().map(|a| a.flag.reason).collect();
        r.sort();
        r
    }

    /// Coverage, pillar C: every detector is tripped by some bot. A detector
    /// no bot trips is a detector nobody has seen fire.
    #[test]
    fn every_flag_fires() {
        let r = standard();
        for reason in FlagReason::ALL {
            assert!(r.bots.iter().any(|b| flagged(b).contains(&reason)), "{} never fired", reason.label());
        }
    }

    #[test]
    fn detector_names_each_cheat_and_nobody_else() {
        let r = standard();
        assert_eq!(flagged(r.bot("aimbot")), vec![FlagReason::Accuracy, FlagReason::AimExact]);
        assert_eq!(flagged(r.bot("speedhack")), vec![FlagReason::AnomalyRate]);
        assert_eq!(flagged(r.bot("burst")), vec![FlagReason::AimExact]);
        for b in r.bots.iter().filter(|b| !["aimbot", "humanized", "speedhack", "burst"].contains(&b.name)) {
            assert!(b.alerts.is_empty(), "{} flagged {:?}", b.name, b.alerts);
        }
    }

    /// Snapshot culling, measured on the real run: the walls hid players from
    /// every bot that stood still or walked (`walled > 0`, so the guard had
    /// something to do), and none of them was ever sent one (`hidden == 0`).
    /// The esp bot plays clean by every guard and detector — culling is the
    /// only thing between it and a wallhack.
    #[test]
    fn culling_sends_no_one_a_player_behind_a_wall() {
        let r = standard();
        for b in &r.bots {
            assert_eq!(b.hidden, 0, "{} was sent {} players it could not see", b.name, b.hidden);
        }
        let esp = r.bot("esp");
        assert!(esp.walled > 0, "no wall ever hid anyone from esp: the test proves nothing");
        assert!(r.bot("honest").walled > 0);
        assert_eq!(esp.totals.total_rejected(), 0);
        assert_eq!(esp.totals.anomalies, 0);
        assert!(esp.alerts.is_empty(), "esp flagged {:?}", esp.alerts);
    }

    /// Why there are two aim detectors: jitter hides the humanized aimbot from
    /// aim_exact, but not from accuracy. And no guard sees it at all.
    #[test]
    fn humanized_aimbot_evades_aim_exact_but_not_accuracy() {
        let r = standard();
        let b = r.bot("humanized");
        assert_eq!(b.totals.total_rejected(), 0);
        assert_eq!(b.totals.anomalies, 0);
        assert_eq!(flagged(b), vec![FlagReason::Accuracy]);
    }

    /// Why the online monitor has a window: the toggle cheater's snapped
    /// shots, averaged over the whole match, stay under the line — offline v0
    /// never flags it. The window does, while the burst is still on, on the
    /// last WINDOW shots rather than the lifetime.
    #[test]
    fn burst_aimbot_escapes_the_match_average_but_not_the_window() {
        let r = standard();
        let b = r.bot("burst");
        let id = b.id.expect("burst joined");
        let v0 = Suite::standard().run(r.telemetry.records());
        assert!(v0.iter().all(|f| f.player != id), "v0 flagged burst: {v0:?}");
        assert_eq!(flagged(b), vec![FlagReason::AimExact]);
        let a = &b.alerts[0];
        assert_eq!(a.flag.samples as usize, WINDOW, "raised by the lifetime, not the window: {a:?}");
        assert!(
            (burst::ON..burst::OFF).contains(&a.tick),
            "raised at tick {}, burst is {}..{}",
            a.tick,
            burst::ON,
            burst::OFF
        );
    }

    /// The false-positive bound: 1008 honest players (63 full arenas of 16 —
    /// walkers, campers, rushers — each with its own aim seed), zero flags:
    /// offline over the whole run, and online at every record, lifetime and
    /// window. "No flags" must not just mean "no verdict": every player is
    /// judged by aim_exact and anomaly_rate, enough of every style reach
    /// accuracy's larger minimum, and enough outlast a window that it slid.
    #[test]
    fn honest_population_is_never_flagged() {
        let players = honest_sweep(63);
        assert_eq!(players.len(), 1008);
        let suite = Suite::standard();
        for p in &players {
            let s = &p.life;
            assert!(s.shots >= aim_exact::MIN_SHOTS, "player {} only {} shots: no verdict", s.player, s.shots);
            assert!(s.accepted >= anomaly_rate::MIN_INPUTS);
            let f = suite.check(s);
            assert!(f.is_empty(), "honest player {} flagged offline {:?}", s.player, f);
            assert!(p.alerts.is_empty(), "honest player {} flagged online {:?}", s.player, p.alerts);
        }
        for style in ["honest", "camper", "rusher"] {
            let judged = players.iter().filter(|p| p.style == style && p.life.shots >= accuracy::MIN_SHOTS).count();
            assert!(judged >= 50, "only {judged} {style}s reached accuracy's {} shots", accuracy::MIN_SHOTS);
        }
        let slid = players.iter().filter(|p| p.life.shots as usize > WINDOW).count();
        assert!(slid >= 150, "only {slid} honest players outlasted a {WINDOW}-shot window");
    }

    /// The honest population is never measured in an emptier arena than the
    /// cheaters play in: hit rate grows with how full the arena is.
    #[test]
    fn the_honest_crowd_is_at_least_as_full_as_the_standard_scenario() {
        assert!(CROWD_SIZE as usize >= Scenario::standard().bots.len());
    }

    /// Why the honest population is a full arena of mixed styles: the esp bot,
    /// culled, is an honest rusher, and in the 15-player standard scenario it
    /// is the honest player most likely to trip accuracy. It does not.
    #[test]
    fn an_honest_rusher_in_a_full_arena_is_not_an_aimbot() {
        let r = standard();
        let esp = r.bot("esp");
        assert!(esp.shots >= accuracy::MIN_SHOTS, "esp only {} shots: no accuracy verdict", esp.shots);
        assert!(esp.accuracy() > 0.6, "esp hit {:.2}: no longer the high honest tail", esp.accuracy());
        assert!(esp.alerts.is_empty(), "esp flagged {:?}", esp.alerts);
    }

    #[test]
    fn aim_evidence_is_in_the_telemetry_stream() {
        // The detector reads telemetry, not the harness counters; the aimbot's
        // snaps must be visible there as near-zero aim_err.
        let r = standard();
        let id = r.bot("aimbot").id.unwrap();
        let errs: Vec<f32> = r
            .telemetry
            .records()
            .iter()
            .filter_map(|rec| match rec.outcome {
                aegis_telemetry::Outcome::Shot { aim_err, .. } if rec.player == id => Some(aim_err),
                _ => None,
            })
            .collect();
        assert_eq!(errs.len() as u32, r.bot("aimbot").shots);
        assert!(errs.iter().filter(|&&e| e < aim_exact::EXACT_RAD).count() * 10 > errs.len() * 9);
    }

    #[test]
    fn with_cookie_answers_only_a_cookieless_join() {
        let join = |cookie| ClientMsg::Join { name: "x".into(), protocol: aegis_protocol::PROTOCOL_VERSION, cookie };
        let retry = with_cookie(&frame(NO_TOKEN, &join(None)), 42).unwrap();
        let (token, body) = split_frame(&retry).unwrap();
        assert_eq!((token, decode::<ClientMsg>(body).unwrap()), (NO_TOKEN, join(Some(42))));
        assert_eq!(with_cookie(&frame(NO_TOKEN, &join(Some(7))), 42), None); // already has one
        let input = ClientMsg::Input { seq: 1, tick: 1, move_dir: Vec2::ZERO, aim: Vec2::ZERO, shoot: false };
        assert_eq!(with_cookie(&frame(9, &input), 42), None);
        assert_eq!(with_cookie(&[1, 2, 3], 42), None); // not even a frame
    }

    #[test]
    fn same_scenario_same_bytes() {
        let (a, b) = (jsonl(&standard().telemetry), jsonl(&standard().telemetry));
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    /// The oracle for the network: the same scenario over real UDP sockets
    /// produces the same telemetry, byte for byte, as the in-process run —
    /// and the same ids, kills, steps and net counters.
    #[test]
    fn udp_run_matches_in_process_byte_for_byte() {
        assert_same_run(&run_udp(Scenario::standard()).unwrap(), &standard());
    }

    /// The oracle through a filtering relay. Every guard still runs at the
    /// origin and the relay only removes traffic the origin would certainly
    /// have refused, so the telemetry is byte-identical to the in-process
    /// run (no player's record changes). What does change is where junk dies:
    /// every datagram the origin no longer counts is one the relay counted
    /// dropping, label by label — nothing vanished unaccounted, and junk
    /// really stopped at the edge (`relay dropped > 0`). And `run_net` already
    /// failed the run if any bot heard from an address but the relay's.
    #[test]
    fn relay_run_keeps_telemetry_and_moves_junk_to_the_edge() {
        let (relay, mem) = (run_relay(Scenario::standard()).unwrap(), standard());
        assert_same_telemetry(&relay, &mem);
        let edge = relay.relay.expect("a relay run reports its relay");
        let dropped = edge_dropped(&edge);
        assert!(edge.bad_token > 0, "the relay dropped nothing at the edge");
        // The token-0 budget at the edge drops exactly the zero-flood's
        // excess — no honest join, and no more than the origin would have —
        // and the origin's source-rate guard is left only the authenticated
        // flood's excess, which the relay cannot judge.
        let over = |per_tick: usize| u64::from((per_tick as u32 - MAX_PER_TICK) * relay.ticks);
        assert_eq!(edge.join_rate, over(zeroflood::PER_TICK));
        assert_eq!(relay.net.get("source_rate"), over(flood::DEFAULT_PER_TICK));
        let total = |n: &NetStats| n.dropped.values().sum::<u64>();
        assert_eq!(total(&mem.net) - total(&relay.net), dropped, "datagrams unaccounted for");
        // The origin saw less of every kind, never more; only what a forged
        // token or an unproven Join causes (the forgery itself, and the IP
        // budget it used to burn) went down.
        for (label, &n) in &mem.net.dropped {
            let at_origin = relay.net.get(label);
            assert!(at_origin <= n, "{label}: origin {at_origin} > in-process {n}");
            if !matches!(*label, "bad_token" | "source_rate" | "malformed_packet" | "bad_cookie") {
                assert_eq!(at_origin, n, "{label} changed");
            }
        }
        assert_eq!(relay.net.get("bad_token"), 0, "a forged token crossed to the origin");
        // Edge cookies: every guessed cookie the origin refused in-process is
        // refused at the relay instead, and none crossed. (That the origin
        // challenged nobody — no cookieless Join crossed — `run_net` already
        // asserted every tick.) The bystander got exactly the bytes it got
        // in-process (`assert_same_telemetry`): challenging at the edge made
        // the relay no better a reflector than the origin was.
        assert!(mem.net.get("bad_cookie") > 0, "no bot guessed a cookie");
        assert_eq!(edge.bad_cookie, mem.net.get("bad_cookie"));
        assert_eq!(relay.net.get("bad_cookie"), 0, "a guessed cookie crossed to the origin");
        assert!(edge.challenged > 0);
    }

    /// The relay's cookie bucket is the origin's: the two clocks count the
    /// same ticks, so a cookie lives as long either way.
    #[test]
    fn the_edge_cookie_bucket_is_the_origins() {
        assert_eq!(aegis_relay::cookie::BUCKET_WINDOWS, aegis_server::guards::cookie::BUCKET_TICKS);
    }

    fn assert_same_run(net: &Report, mem: &Report) {
        assert_same_telemetry(net, mem);
        assert_eq!(net.net, mem.net);
    }

    fn assert_same_telemetry(net: &Report, mem: &Report) {
        let (a, b) = (jsonl(&net.telemetry), jsonl(&mem.telemetry));
        if a != b {
            let (a, b) = (String::from_utf8(a).unwrap(), String::from_utf8(b).unwrap());
            let (line, (u, m)) = a.lines().zip(b.lines()).enumerate().find(|(_, (u, m))| u != m).unwrap_or_default();
            panic!("telemetry differs at line {}:\n  net:        {u}\n  in-process: {m}", line + 1);
        }
        assert_eq!(net.bystander, mem.bystander);
        for (u, m) in net.bots.iter().zip(&mem.bots) {
            assert_eq!((u.name, u.id, u.kills, u.max_step), (m.name, m.id, m.kills, m.max_step));
            // Same records -> same alerts at the same ticks. Asserted anyway:
            // a detector verdict is what a reviewer acts on.
            assert_eq!(u.alerts, m.alerts, "{} alerts differ", u.name);
        }
        assert!(mem.bots.iter().any(|b| !b.alerts.is_empty()), "no alerts: the comparison proves nothing");
    }

    /// The before/after of hiding the origin. Without a relay, a client
    /// aiming at the origin's address is just a client: it joins and plays.
    /// Behind one, every datagram it sends is dropped unread (`not_relay`
    /// counts each), it never joins, and nothing it does reaches telemetry —
    /// while the honest player beside it, through the relay, plays as before.
    #[test]
    fn a_client_aiming_at_the_origin_is_cut_off_only_behind_a_relay() {
        let open = run_udp(Scenario::relay_probe()).unwrap();
        assert!(open.bot("direct").joined());
        assert!(open.bot("direct").totals.accepted > 0);
        assert_eq!(open.net.get(NOT_RELAY), 0);

        let hid = run_relay(Scenario::relay_probe()).unwrap();
        let d = hid.bot("direct");
        assert!(!d.joined());
        assert_eq!(d.totals, Totals::default());
        // its join, then one input per tick
        assert_eq!(hid.net.get(NOT_RELAY), 1 + u64::from(hid.ticks));
        let h = hid.bot("honest");
        assert!(h.joined());
        assert_eq!(h.totals.accepted, open.bot("honest").totals.accepted);
    }

    /// The cull sweep's edges. Margin 0 is the exact view: it leaks nothing
    /// and a client with no lag is never late. A lagging client on it does
    /// see pop-in (else there is nothing to trade), and a wider margin only
    /// ever leaks more and is late less.
    #[test]
    fn cull_sweep_edges() {
        let s = cull_sweep(5, 2);
        assert!(s.visible > 0 && s.walled > 0, "test setup: nobody was ever walled");
        assert_eq!(s.leaked[0], 0);
        assert!(s.late.iter().all(|row| row[0] == 0), "lag 0 can never be late");
        assert!(s.late[0][1] > 0, "margin 0 never popped in: the trade measures nothing");
        for k in 1..s.leaked.len() {
            assert!(s.leaked[k] >= s.leaked[k - 1]);
            assert!((0..s.late[k].len()).all(|l| s.late[k][l] <= s.late[k - 1][l]));
        }
    }

    /// The rescaled sweep agrees with the real one at the real step, leaks
    /// nothing at margin 0 whatever the step, and a smaller step leaks less.
    #[test]
    fn leak_by_step_edges() {
        let (walled, leaked) = leak_by_step(3, &[MOVE_SPEED, 0.1], 2);
        assert!(walled > 0);
        assert_eq!(leaked[0][0], 0);
        assert_eq!(leaked[1][0], 0);
        assert!(leaked[0][1] > 0, "test setup: the real step should leak at margin 1");
        assert!((1..=2).all(|k| leaked[1][k] < leaked[0][k]));
        // same lobbies, same step: what cull_sweep counts from tick 2 on is
        // a subset of what this counts from tick 0
        assert!(cull_sweep(3, 2).leaked[1] <= leaked[0][1]);
    }
}
