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

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use aegis_client_sdk::{
    aimbot::AimbotBot, badversion::BadVersionBot, flood::FloodBot, garbage::GarbageBot,
    honest::HonestBot, humanized::HumanizedAimbot, nan::NanBot, replay::ReplayBot,
    speedhack::SpeedhackBot, Bot, BotCtx,
};
use aegis_detector::{Flag, Suite};
use aegis_protocol::{decode, encode, PlayerId, PlayerState, ServerMsg, Vec2};
use aegis_server::net::MAX_DATAGRAM;
use aegis_server::{NetServer, NetStats, Server, TickOutcome};
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
            ],
        }
    }

    /// A lobby of `LOBBY_SIZE` honest players, each with its own aim seed
    /// derived from `seed`. Nobody here cheats, so any flag the detector
    /// raises in it is a false positive.
    pub fn honest_lobby(seed: u32) -> Self {
        Self {
            name: "honest_lobby",
            ticks: 300,
            bots: (0..LOBBY_SIZE)
                .map(|k| Box::new(HonestBot::with_seed(seed.wrapping_mul(LOBBY_SIZE).wrapping_add(k + 1))) as Box<dyn Bot>)
                .collect(),
        }
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
    /// What the detector suite raised on this player's telemetry.
    pub flags: Vec<Flag>,
}

pub struct Report {
    pub scenario: &'static str,
    pub ticks: u32,
    pub bots: Vec<BotReport>,
    pub telemetry: Telemetry,
    /// Datagrams dropped with no player to pin them on (source-rate drops,
    /// anything from an address that never joined).
    pub net: NetStats,
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

/// Detector stats for every player across `lobbies` honest lobbies (seeds
/// 0..lobbies) — the honest population the thresholds are measured against.
pub fn honest_sweep(lobbies: u32) -> Vec<aegis_detector::PlayerStats> {
    (0..lobbies)
        .flat_map(|seed| aegis_detector::stats(run(Scenario::honest_lobby(seed)).telemetry.records()).into_values())
        .collect()
}

/// What the harness tracks per bot while the scenario runs, whichever
/// transport carries the bytes. Indexed by bot slot.
struct Lab {
    ids: Vec<Option<PlayerId>>,
    kills: Vec<u32>,
    max_step: Vec<f32>,
}

impl Lab {
    fn new(n: usize) -> Self {
        Self { ids: vec![None; n], kills: vec![0; n], max_step: vec![0.0; n] }
    }

    fn ctx<'a>(&self, i: usize, tick: u32, snapshot: &'a [PlayerState]) -> BotCtx<'a> {
        // An unadmitted bot has no id; 0 is never issued.
        BotCtx { tick, my_id: self.ids[i].unwrap_or(0), snapshot }
    }

    fn slot(&self, p: PlayerId) -> usize {
        self.ids.iter().position(|&x| x == Some(p)).expect("outcome for a player no bot owns")
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
        let stats = aegis_detector::stats(tel.records());
        let suite = Suite::standard();
        let bots = sc
            .bots
            .iter()
            .enumerate()
            .map(|(i, bot)| {
                let id = self.ids[i];
                let totals = id.map(|p| tel.per_player(p)).unwrap_or_default();
                let flags = id.and_then(|p| stats.get(&p)).map(|s| suite.check(s)).unwrap_or_default();
                BotReport {
                    name: bot.name(),
                    id,
                    shots: totals.shots,
                    hits: totals.hits,
                    totals,
                    kills: self.kills[i],
                    max_step: self.max_step[i],
                    flags,
                }
            })
            .collect();
        Report { scenario: sc.name, ticks: sc.ticks, bots, telemetry: tel, net }
    }
}

/// Run a scenario in-process: the server is called directly with each bot's
/// address. Deterministic.
pub fn run(mut sc: Scenario) -> Report {
    let n = sc.bots.len();
    let addr = |i: usize| SocketAddr::from((bot_ip(i), 40000));
    let mut server = Server::new(spawns(n));
    let mut lab = Lab::new(n);

    // Tick 0: the handshake, over the wire like everything else.
    for (i, bot) in sc.bots.iter().enumerate() {
        lab.ids[i] = server.receive(0, addr(i), &encode(&bot.join()));
    }

    for tick in 1..=sc.ticks {
        let snapshot = server.begin_tick();
        for (i, bot) in sc.bots.iter_mut().enumerate() {
            // The server sends snapshots to admitted players only.
            let seen: &[PlayerState] = if lab.ids[i].is_some() { &snapshot } else { &[] };
            for d in bot.datagrams(&lab.ctx(i, tick, seen)) {
                if let Some(id) = server.receive(tick, addr(i), &d) {
                    lab.ids[i] = Some(id);
                }
            }
        }
        lab.measure(server.end_tick(tick));
    }

    let (tel, net) = server.into_parts();
    lab.report(&sc, tel, net)
}

/// Run a scenario over real UDP on loopback: a [`NetServer`] and one socket
/// per bot. Lockstep, not real-time — each tick the server reads exactly as
/// many datagrams as the bots sent — so the result does not depend on timing.
/// What a bot sees (its id, the snapshot) comes off the wire.
pub fn run_udp(mut sc: Scenario) -> io::Result<Report> {
    let n = sc.bots.len();
    let mut net = NetServer::bind((Ipv4Addr::LOCALHOST, 0), Server::new(spawns(n)))?;
    let to = net.local_addr()?;
    let socks = (0..n)
        .map(|i| {
            let s = UdpSocket::bind((bot_ip(i), 0))?;
            s.set_read_timeout(Some(UDP_WAIT))?;
            Ok(s)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut lab = Lab::new(n);

    for (i, bot) in sc.bots.iter().enumerate() {
        socks[i].send_to(&encode(&bot.join()), to)?;
    }
    pump(&mut net, n)?;

    for tick in 1..=sc.ticks {
        net.begin_tick()?;
        let mut sent = 0;
        for (i, bot) in sc.bots.iter_mut().enumerate() {
            // The server's word on who is admitted decides only whether to
            // wait for a snapshot; what is in it is read off the socket.
            let snapshot = match net.server().player_id(socks[i].local_addr()?) {
                Some(_) => read_snapshot(&socks[i], tick, &mut lab.ids[i])?,
                None => Vec::new(),
            };
            for d in bot.datagrams(&lab.ctx(i, tick, &snapshot)) {
                socks[i].send_to(&d, to)?;
                sent += 1;
            }
        }
        pump(&mut net, sent)?;
        lab.measure(net.end_tick());
    }

    let (tel, stats) = net.into_server().into_parts();
    Ok(lab.report(&sc, tel, stats))
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

/// Read this tick's snapshot, taking any `Joined` that arrives first.
fn read_snapshot(sock: &UdpSocket, tick: u32, id: &mut Option<PlayerId>) -> io::Result<Vec<PlayerState>> {
    let mut buf = [0u8; MAX_DATAGRAM];
    loop {
        let n = sock.recv(&mut buf)?;
        match decode::<ServerMsg>(&buf[..n]) {
            Ok(ServerMsg::Joined { player_id, .. }) => *id = Some(player_id),
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
    use aegis_client_sdk::{flood, garbage};
    use aegis_server::guards::source_rate::MAX_PER_TICK;
    use aegis_server::sim::MOVE_SPEED;
    use aegis_detector::detectors::{accuracy, aim_exact, anomaly_rate};
    use aegis_detector::FlagReason;
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
        let per_tick = flood::DEFAULT_PER_TICK as u32;
        assert_eq!(b.totals.accepted, r.ticks);
        assert_eq!(rejected(b, "rate_exceeded"), (MAX_PER_TICK - 1) * r.ticks);
        assert_eq!(b.totals.total_rejected(), (MAX_PER_TICK - 1) * r.ticks);
        assert_eq!(r.net.get("source_rate"), u64::from((per_tick - MAX_PER_TICK) * r.ticks));
    }

    #[test]
    fn replay_gets_only_its_first_input() {
        let r = standard();
        let b = r.bot("replay");
        assert_eq!(b.totals.accepted, 1);
        assert_eq!(rejected(b, "replay"), r.ticks - 1);
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

    fn flagged(b: &BotReport) -> Vec<FlagReason> {
        b.flags.iter().map(|f| f.reason).collect()
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
        for b in r.bots.iter().filter(|b| !["aimbot", "humanized", "speedhack"].contains(&b.name)) {
            assert!(b.flags.is_empty(), "{} flagged {:?}", b.name, b.flags);
        }
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

    /// The false-positive bound: 1000 honest players (250 lobbies of 4, each
    /// with its own aim seed), zero flags. Every one of them must also have
    /// enough shots to be judged, or "no flags" would only mean "no verdict".
    #[test]
    fn honest_population_is_never_flagged() {
        let players = honest_sweep(250);
        assert_eq!(players.len(), 1000);
        let suite = Suite::standard();
        for s in &players {
            assert!(s.shots() >= accuracy::MIN_SHOTS, "player {} only {} shots: no verdict", s.player, s.shots());
            assert!(s.accepted >= anomaly_rate::MIN_INPUTS);
            let f = suite.check(s);
            assert!(f.is_empty(), "honest player {} flagged {:?}", s.player, f);
        }
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
        let (mem, udp) = (standard(), run_udp(Scenario::standard()).unwrap());
        let (a, b) = (jsonl(&udp.telemetry), jsonl(&mem.telemetry));
        if a != b {
            let (a, b) = (String::from_utf8(a).unwrap(), String::from_utf8(b).unwrap());
            let (line, (u, m)) = a.lines().zip(b.lines()).enumerate().find(|(_, (u, m))| u != m).unwrap_or_default();
            panic!("telemetry differs at line {}:\n  udp:        {u}\n  in-process: {m}", line + 1);
        }
        assert_eq!(udp.net, mem.net);
        for (u, m) in udp.bots.iter().zip(&mem.bots) {
            assert_eq!((u.name, u.id, u.kills, u.max_step), (m.name, m.id, m.kills, m.max_step));
        }
    }
}
