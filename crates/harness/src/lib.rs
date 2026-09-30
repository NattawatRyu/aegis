//! Aegis harness — the lab bench.
//!
//! Puts every bot in one arena, runs N ticks against the real guards and the
//! real sim, and reports per bot what was accepted, flagged and rejected. It is
//! how "the speedhack is blocked" becomes a number instead of a claim.
//!
//! Everything goes through the wire: each bot's messages are encoded to bytes
//! and decoded by the packet guard, exactly as a UDP server would receive them.
//! The bot's slot stands in for the source address, so a datagram that fails
//! to decode is still attributed to the player who sent it.
//!
//! In-process and deterministic (no sockets, no clock, no RNG): two runs of the
//! same scenario produce byte-identical telemetry. The dispatch in [`World`]
//! is server logic; it moves into the server crate when the UDP loop arrives
//! (pillar D).

use std::collections::BTreeSet;

use aegis_client_sdk::{
    aimbot::AimbotBot, badversion::BadVersionBot, flood::FloodBot, garbage::GarbageBot,
    honest::HonestBot, humanized::HumanizedAimbot, nan::NanBot, replay::ReplayBot,
    speedhack::SpeedhackBot, Bot, BotCtx,
};
use aegis_detector::{Flag, Suite};
use aegis_protocol::{encode, ClientMsg, PlayerId, Vec2};
use aegis_server::guards::{joined, packet, version};
use aegis_server::{ClientInput, GuardCtx, GuardVerdict, Pipeline, Sim};
use aegis_telemetry::{Telemetry, Totals};

/// Bots spawn evenly on a circle this far from the center — inside every
/// bot's shot range, so the aimbot always has targets.
pub const SPAWN_RADIUS: f32 = 20.0;

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
    pub id: PlayerId,
    pub name: &'static str,
    /// Join passed the version guard and the player was spawned.
    pub joined: bool,
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
}

impl BotReport {
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

/// Server-side state for one run.
struct World {
    sim: Sim,
    pipe: Pipeline,
    tel: Telemetry,
    joined: BTreeSet<PlayerId>,
}

impl World {
    /// Receive one datagram from `player`. Returns the input to fold into the
    /// sim if it got through every guard. Accepted joins are not recorded:
    /// telemetry is the per-input stream the detector learns from.
    fn receive(&mut self, tick: u32, player: PlayerId, bytes: &[u8], spawn: Vec2) -> Option<ClientInput> {
        let msg = match packet::decode_client(bytes) {
            Ok(m) => m,
            Err(r) => return self.reject(tick, player, r.label()),
        };
        match msg {
            ClientMsg::Join { protocol, .. } => {
                match version::check_join(protocol) {
                    Ok(()) if self.joined.insert(player) => self.sim.spawn(player, spawn),
                    Ok(()) => {} // already in: a repeated join changes nothing
                    Err(r) => {
                        self.tel.reject(tick, player, r.label());
                    }
                }
                None
            }
            ClientMsg::Input { seq, tick: client_tick, move_dir, aim, shoot } => {
                if let Err(r) = joined::check_input(&self.joined, player) {
                    return self.reject(tick, player, r.label());
                }
                let mut input = ClientInput { seq, tick: client_tick, move_dir, aim, shoot };
                match self.pipe.run(&GuardCtx { tick, player }, &mut input) {
                    GuardVerdict::Ok { anomaly } => {
                        self.tel.accept(tick, player, anomaly);
                        Some(input)
                    }
                    GuardVerdict::Rejected(r) => self.reject(tick, player, r.label()),
                }
            }
        }
    }

    fn reject(&mut self, tick: u32, player: PlayerId, reason: &'static str) -> Option<ClientInput> {
        self.tel.reject(tick, player, reason);
        None
    }
}

fn spawn_pos(i: usize, n: usize) -> Vec2 {
    let a = std::f32::consts::TAU * i as f32 / n as f32;
    Vec2::new(SPAWN_RADIUS * a.cos(), SPAWN_RADIUS * a.sin())
}

fn dist(a: Vec2, b: Vec2) -> f32 {
    Vec2::new(a.x - b.x, a.y - b.y).len()
}

/// Detector stats for every player across `lobbies` honest lobbies (seeds
/// 0..lobbies) — the honest population the thresholds are measured against.
pub fn honest_sweep(lobbies: u32) -> Vec<aegis_detector::PlayerStats> {
    (0..lobbies)
        .flat_map(|seed| aegis_detector::stats(run(Scenario::honest_lobby(seed)).telemetry.records()).into_values())
        .collect()
}

pub fn run(mut sc: Scenario) -> Report {
    let n = sc.bots.len();
    let id = |i: usize| (i + 1) as PlayerId;
    let spawns: Vec<Vec2> = (0..n).map(|i| spawn_pos(i, n)).collect();
    let mut w = World { sim: Sim::new(), pipe: Pipeline::standard(), tel: Telemetry::new(), joined: BTreeSet::new() };
    let mut kills = vec![0u32; n];
    let mut max_step = vec![0f32; n];

    // Tick 0: the handshake, over the wire like everything else.
    for (i, bot) in sc.bots.iter().enumerate() {
        w.receive(0, id(i), &encode(&bot.join()), spawns[i]);
    }

    for tick in 1..=sc.ticks {
        w.sim.step_respawns();
        let snapshot = w.sim.snapshot();
        let mut accepted: Vec<(usize, ClientInput)> = Vec::new();
        for (i, bot) in sc.bots.iter_mut().enumerate() {
            let ctx = BotCtx { tick, my_id: id(i), snapshot: &snapshot };
            for d in bot.datagrams(&ctx) {
                if let Some(input) = w.receive(tick, id(i), &d, spawns[i]) {
                    accepted.push((i, input));
                }
            }
        }

        // Shots resolve against the world the clients were shown (nobody has
        // moved yet this tick), then everyone moves.
        // Every shot resolves; only shots that say something about aim are
        // recorded. No enemy, or an enemy point-blank, leaves no evidence
        // either way (see `Sim::aim_error`).
        for &(i, input) in &accepted {
            if !input.shoot {
                continue;
            }
            let err = w.sim.aim_error(id(i), input.aim);
            let r = w.sim.apply_shot(id(i), input.aim);
            kills[i] += r.is_some_and(|r| r.killed) as u32;
            if let Some(err) = err {
                w.tel.shot(tick, id(i), r.is_some(), err);
            }
        }
        for &(i, input) in &accepted {
            let before = w.sim.player(id(i)).map(|p| p.pos);
            w.sim.apply_move(id(i), input.move_dir);
            if let (Some(a), Some(b)) = (before, w.sim.player(id(i)).map(|p| p.pos)) {
                max_step[i] = max_step[i].max(dist(a, b));
            }
        }
    }

    let stats = aegis_detector::stats(w.tel.records());
    let suite = Suite::standard();
    let bots = sc
        .bots
        .iter()
        .enumerate()
        .map(|(i, bot)| {
            let totals = w.tel.per_player(id(i));
            let flags = stats.get(&id(i)).map(|s| suite.check(s)).unwrap_or_default();
            BotReport {
                id: id(i),
                name: bot.name(),
                joined: w.joined.contains(&id(i)),
                shots: totals.shots,
                hits: totals.hits,
                totals,
                kills: kills[i],
                max_step: max_step[i],
                flags,
            }
        })
        .collect();
    Report { scenario: sc.name, ticks: sc.ticks, bots, telemetry: w.tel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_client_sdk::garbage;
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

    #[test]
    fn honest_passes_clean() {
        let r = standard();
        let b = r.bot("honest");
        assert!(b.joined);
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

    #[test]
    fn flood_gets_exactly_one_input_per_tick() {
        let r = standard();
        let b = r.bot("flood");
        assert_eq!(b.totals.accepted, r.ticks);
        // FloodBot::default sends 50 per tick; all but the first are rejected.
        assert_eq!(rejected(b, "rate_exceeded"), 49 * r.ticks);
        assert_eq!(b.totals.total_rejected(), 49 * r.ticks);
    }

    #[test]
    fn replay_gets_only_its_first_input() {
        let r = standard();
        let b = r.bot("replay");
        assert_eq!(b.totals.accepted, 1);
        assert_eq!(rejected(b, "replay"), r.ticks - 1);
    }

    #[test]
    fn badversion_never_spawns_and_every_input_is_refused() {
        let r = standard();
        let b = r.bot("badversion");
        assert!(!b.joined);
        assert_eq!(rejected(b, "bad_version"), 1);
        assert_eq!(rejected(b, "not_joined"), r.ticks);
        assert_eq!(b.totals.accepted, 0);
        assert!(r.telemetry.records().iter().all(|rec| rec.player != b.id || rec.tick == 0
            || matches!(rec.outcome, aegis_telemetry::Outcome::Rejected { reason: "not_joined" })));
    }

    #[test]
    fn garbage_is_dropped_at_decode() {
        let r = standard();
        let b = r.bot("garbage");
        assert!(b.joined); // the join was legal: only the packet guard stands in the way
        assert_eq!(b.totals.accepted, 0);
        assert_eq!(rejected(b, "malformed_packet"), garbage::SHAPES as u32 * r.ticks);
    }

    #[test]
    fn nan_is_stopped_by_sanity_before_the_sim() {
        let r = standard();
        let b = r.bot("nan");
        assert!(b.joined);
        assert_eq!(b.totals.accepted, 0);
        assert_eq!(rejected(b, "malformed_input"), r.ticks);
        assert_eq!(b.shots, 0); // a NaN aim never reached apply_shot
    }

    /// Coverage: every reject reason the server has is produced by some bot in
    /// the standard scenario. A guard no bot trips is a guard nobody has seen
    /// fire.
    #[test]
    fn every_reject_reason_fires() {
        let r = standard();
        let all = r.telemetry.totals();
        for reason in RejectReason::ALL {
            assert!(all.rejected.get(reason.label()).copied().unwrap_or(0) > 0, "{} never fired", reason.label());
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
        let id = r.bot("aimbot").id;
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
        let (mut a, mut b) = (Vec::new(), Vec::new());
        standard().telemetry.write_jsonl(&mut a).unwrap();
        standard().telemetry.write_jsonl(&mut b).unwrap();
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }
}
