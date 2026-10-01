//! The server's tick: everything between "a datagram arrived" and "the world
//! moved". Transport-agnostic — whoever owns the socket (the UDP loop in
//! [`crate::net`], or the in-process harness with made-up addresses) calls,
//! per tick:
//!
//! ```text
//! begin_tick  -> respawns, then the snapshot clients act on
//! receive     -> once per datagram:
//!                source_rate -> who is it? -> decode -> version / pipeline
//! end_tick    -> shots against the pre-move world, then moves
//! ```
//!
//! Identity is the source address. A player id is issued only when a legal
//! Join arrives from a new address; everything else from an unknown address is
//! counted in [`NetStats`] and dropped. Both transports run this same code, so
//! an in-process run and a UDP run of the same scenario are directly
//! comparable.
//!
//! Every verdict and every aim-evidence shot of an admitted player is written
//! to [`Telemetry`]: that stream is what the detector (pillar C) reads.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use aegis_protocol::{ClientMsg, PlayerId, PlayerState, Vec2};
use aegis_telemetry::Telemetry;

use crate::guards::source_rate::SourceRate;
use crate::guards::{joined, packet, version};
use crate::{ClientInput, GuardCtx, GuardVerdict, Pipeline, RejectReason, Sim};

/// [`NetStats`] label for a legal join refused because all 255 ids are taken.
pub const SERVER_FULL: &str = "server_full";

/// What one tick did, for callers that measure the world (the lab's kill
/// count and movement-authority check). The server itself keeps no metrics.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TickOutcome {
    /// One entry per kill, naming the killer.
    pub kills: Vec<PlayerId>,
    /// One entry per applied move: who, and how far they actually travelled.
    pub steps: Vec<(PlayerId, f32)>,
}

/// Datagrams dropped without a player to pin them on: source-rate drops
/// (counted before anyone looks at who sent them) and anything from an
/// address that never joined. Counters only — bounded no matter how hard the
/// server is flooded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NetStats {
    pub dropped: BTreeMap<&'static str, u64>,
}

impl NetStats {
    fn drop(&mut self, label: &'static str) {
        *self.dropped.entry(label).or_insert(0) += 1;
    }

    pub fn get(&self, label: &str) -> u64 {
        self.dropped.get(label).copied().unwrap_or(0)
    }
}

pub struct Server {
    sim: Sim,
    pipe: Pipeline,
    tel: Telemetry,
    net: NetStats,
    source_rate: SourceRate,
    /// Admitted source -> its player id. Ids are 1.. in join order, never reused.
    sources: BTreeMap<SocketAddr, PlayerId>,
    /// Player `id` enters at `spawns[(id - 1) % len]`.
    spawns: Vec<Vec2>,
    /// Inputs accepted this tick, in arrival order, waiting for `end_tick`.
    pending: Vec<(PlayerId, ClientInput)>,
}

impl Server {
    /// A server whose players enter at `spawns`, in join order (wrapping).
    pub fn new(spawns: Vec<Vec2>) -> Self {
        assert!(!spawns.is_empty(), "a server needs at least one spawn point");
        Self {
            sim: Sim::new(),
            pipe: Pipeline::standard(),
            tel: Telemetry::new(),
            net: NetStats::default(),
            source_rate: SourceRate::new(),
            sources: BTreeMap::new(),
            spawns,
            pending: Vec::new(),
        }
    }

    /// Start a tick: advance respawn timers, then return the world as the
    /// clients will see it while choosing this tick's input.
    pub fn begin_tick(&mut self) -> Vec<PlayerState> {
        self.sim.step_respawns();
        self.sim.snapshot()
    }

    /// Receive one datagram from `from`. An input that passes every guard is
    /// queued for `end_tick`; everything else is recorded and dropped.
    ///
    /// Returns `Some(id)` when the datagram was a legal Join — a new player or
    /// a repeat from an admitted one — so the transport can answer
    /// `ServerMsg::Joined` (a repeat means the last answer was lost).
    pub fn receive(&mut self, tick: u32, from: SocketAddr, bytes: &[u8]) -> Option<PlayerId> {
        if let Err(r) = self.source_rate.check(tick, from.ip()) {
            self.net.drop(r.label());
            return None;
        }
        match joined::check_source(&self.sources, from) {
            Ok(player) => self.receive_from(tick, player, bytes),
            Err(not_joined) => self.admit(from, bytes, not_joined),
        }
    }

    /// A datagram from an unknown address: only a legal Join gets through,
    /// and it is what creates the player.
    fn admit(&mut self, from: SocketAddr, bytes: &[u8], not_joined: RejectReason) -> Option<PlayerId> {
        let refused = match packet::decode_client(bytes) {
            Ok(ClientMsg::Join { protocol, .. }) => version::check_join(protocol).err().map(RejectReason::label),
            Ok(ClientMsg::Input { .. }) => Some(not_joined.label()),
            Err(r) => Some(r.label()),
        };
        if let Some(label) = refused {
            self.net.drop(label);
            return None;
        }
        let Ok(id) = PlayerId::try_from(self.sources.len() + 1) else {
            self.net.drop(SERVER_FULL);
            return None;
        };
        self.sources.insert(from, id);
        self.sim.spawn(id, self.spawns[(id as usize - 1) % self.spawns.len()]);
        Some(id)
    }

    /// A datagram from an admitted player: everything is on their record.
    fn receive_from(&mut self, tick: u32, player: PlayerId, bytes: &[u8]) -> Option<PlayerId> {
        let msg = match packet::decode_client(bytes) {
            Ok(m) => m,
            Err(r) => {
                self.tel.reject(tick, player, r.label());
                return None;
            }
        };
        match msg {
            // Already in: a repeated join changes nothing but is answered again.
            ClientMsg::Join { protocol, .. } => match version::check_join(protocol) {
                Ok(()) => Some(player),
                Err(r) => {
                    self.tel.reject(tick, player, r.label());
                    None
                }
            },
            ClientMsg::Input { seq, tick: client_tick, move_dir, aim, shoot } => {
                let mut input = ClientInput { seq, tick: client_tick, move_dir, aim, shoot };
                match self.pipe.run(&GuardCtx { tick, player }, &mut input) {
                    GuardVerdict::Ok { anomaly } => {
                        self.tel.accept(tick, player, anomaly);
                        self.pending.push((player, input));
                    }
                    GuardVerdict::Rejected(r) => self.tel.reject(tick, player, r.label()),
                }
                None
            }
        }
    }

    /// Fold this tick's accepted inputs into the world. Shots resolve against
    /// the world the clients were shown (nobody has moved yet), then everyone
    /// moves. Every shot resolves; only shots that say something about aim are
    /// recorded — no enemy, or one point-blank, is no evidence either way
    /// (see [`Sim::aim_error`]).
    pub fn end_tick(&mut self, tick: u32) -> TickOutcome {
        let pending = std::mem::take(&mut self.pending);
        let mut out = TickOutcome::default();
        for &(p, input) in &pending {
            if !input.shoot {
                continue;
            }
            let err = self.sim.aim_error(p, input.aim);
            let r = self.sim.apply_shot(p, input.aim);
            if r.is_some_and(|r| r.killed) {
                out.kills.push(p);
            }
            if let Some(err) = err {
                self.tel.shot(tick, p, r.is_some(), err);
            }
        }
        for &(p, input) in &pending {
            let before = self.sim.player(p).map(|s| s.pos);
            self.sim.apply_move(p, input.move_dir);
            if let (Some(a), Some(b)) = (before, self.sim.player(p).map(|s| s.pos)) {
                out.steps.push((p, Vec2::new(a.x - b.x, a.y - b.y).len()));
            }
        }
        out
    }

    /// The player admitted from `from`, if any.
    pub fn player_id(&self, from: SocketAddr) -> Option<PlayerId> {
        self.sources.get(&from).copied()
    }

    /// Every admitted source and its player — who a snapshot goes to.
    pub fn peers(&self) -> impl Iterator<Item = (SocketAddr, PlayerId)> + '_ {
        self.sources.iter().map(|(&a, &p)| (a, p))
    }

    pub fn sim(&self) -> &Sim {
        &self.sim
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.tel
    }

    pub fn net_stats(&self) -> &NetStats {
        &self.net
    }

    /// For the transport: a datagram it had to drop before `receive` could
    /// see it (see [`crate::net::OVERSIZE`]).
    pub(crate) fn count_drop(&mut self, label: &'static str) {
        self.net.drop(label);
    }

    pub fn into_parts(self) -> (Telemetry, NetStats) {
        (self.tel, self.net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guards::source_rate::MAX_PER_TICK;
    use crate::sim::{MAX_HEALTH, MOVE_SPEED, SHOT_DAMAGE};
    use aegis_protocol::{encode, PROTOCOL_VERSION};
    use aegis_telemetry::Outcome;

    fn addr(n: u8) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, n], 4000))
    }

    fn join(protocol: u16) -> Vec<u8> {
        encode(&ClientMsg::Join { name: "t".into(), protocol })
    }

    fn input(seq: u32, move_dir: Vec2, aim: Vec2, shoot: bool) -> Vec<u8> {
        encode(&ClientMsg::Input { seq, tick: seq, move_dir, aim, shoot })
    }

    fn server() -> Server {
        Server::new(vec![Vec2::ZERO, Vec2::new(10.0, 0.0)])
    }

    /// Player 1 at the origin, player 2 at (10, 0).
    fn joined_pair() -> Server {
        let mut s = server();
        assert_eq!(s.receive(0, addr(1), &join(PROTOCOL_VERSION)), Some(1));
        assert_eq!(s.receive(0, addr(2), &join(PROTOCOL_VERSION)), Some(2));
        s
    }

    #[test]
    fn legal_join_spawns_once_and_is_not_recorded() {
        let mut s = server();
        assert_eq!(s.receive(0, addr(1), &join(PROTOCOL_VERSION)), Some(1));
        // a repeat is answered again (the reply may have been lost) but changes nothing
        assert_eq!(s.receive(0, addr(1), &join(PROTOCOL_VERSION)), Some(1));
        assert_eq!(s.player_id(addr(1)), Some(1));
        assert_eq!(s.sim().snapshot().len(), 1);
        assert!(s.telemetry().is_empty());
    }

    #[test]
    fn ids_follow_legal_join_order_and_spawns_wrap() {
        let mut s = server();
        assert_eq!(s.receive(0, addr(1), &join(PROTOCOL_VERSION)), Some(1));
        assert_eq!(s.receive(0, addr(2), &join(PROTOCOL_VERSION + 1)), None); // refused: no id spent
        assert_eq!(s.receive(0, addr(3), &join(PROTOCOL_VERSION)), Some(2));
        assert_eq!(s.receive(0, addr(4), &join(PROTOCOL_VERSION)), Some(3));
        assert_eq!(s.sim().player(2).unwrap().pos, Vec2::new(10.0, 0.0));
        assert_eq!(s.sim().player(3).unwrap().pos, Vec2::ZERO);
    }

    #[test]
    fn bad_version_join_is_counted_and_never_spawns() {
        let mut s = server();
        assert_eq!(s.receive(0, addr(1), &join(PROTOCOL_VERSION + 1)), None);
        assert_eq!(s.player_id(addr(1)), None);
        assert!(s.sim().snapshot().is_empty());
        assert!(s.telemetry().is_empty()); // no id, so nothing per-player
        assert_eq!(s.net_stats().get("bad_version"), 1);
    }

    #[test]
    fn unknown_source_never_gets_an_id_or_a_record() {
        let mut s = server();
        s.receive(1, addr(1), &input(1, Vec2::new(1.0, 0.0), Vec2::new(1.0, 0.0), true));
        s.receive(1, addr(1), &[0xFF, 0xFF, 0xFF]);
        assert_eq!(s.end_tick(1), TickOutcome::default());
        assert_eq!(s.peers().count(), 0);
        assert!(s.telemetry().is_empty());
        assert_eq!((s.net_stats().get("not_joined"), s.net_stats().get("malformed_packet")), (1, 1));
    }

    /// The other half of "identity is the address": an admitted player's
    /// address on another port is somebody else, and gets nothing.
    #[test]
    fn same_ip_other_port_is_not_the_player() {
        let mut s = joined_pair();
        let other = SocketAddr::from(([10, 0, 0, 1], 4001));
        s.receive(1, other, &input(1, Vec2::new(1.0, 0.0), Vec2::ZERO, false));
        assert_eq!(s.end_tick(1), TickOutcome::default());
        assert_eq!(s.net_stats().get("not_joined"), 1);
    }

    #[test]
    fn garbage_from_a_player_is_on_their_record() {
        let mut s = joined_pair();
        s.receive(1, addr(2), &[0xFF, 0xFF, 0xFF]);
        let r = &s.telemetry().records()[0];
        assert_eq!((r.player, &r.outcome), (2, &Outcome::Rejected { reason: "malformed_packet" }));
    }

    /// The edge: datagram MAX_PER_TICK from one IP is decoded, the next is
    /// not — it never reaches the pipeline, so input_rate never sees it and
    /// there is no record of it, only a counter.
    #[test]
    fn source_rate_drops_past_the_cap_before_decode() {
        let mut s = joined_pair();
        for seq in 1..=MAX_PER_TICK + 1 {
            s.receive(1, addr(1), &input(seq, Vec2::ZERO, Vec2::ZERO, false));
        }
        let t = s.telemetry().per_player(1);
        assert_eq!((t.accepted, t.rejected.get("rate_exceeded").copied()), (1, Some(MAX_PER_TICK - 1)));
        assert_eq!(s.net_stats().get("source_rate"), 1);
        assert_eq!(s.telemetry().len(), MAX_PER_TICK as usize);
    }

    #[test]
    fn unjoined_flood_costs_at_most_the_cap_in_decodes() {
        let mut s = server();
        for _ in 0..100 {
            s.receive(1, addr(9), &input(1, Vec2::ZERO, Vec2::ZERO, false));
        }
        assert_eq!(s.net_stats().get("not_joined"), MAX_PER_TICK as u64);
        assert_eq!(s.net_stats().get("source_rate"), 100 - MAX_PER_TICK as u64);
    }

    #[test]
    fn the_256th_player_is_refused() {
        let mut s = server();
        let mut ids = Vec::new();
        for n in 0..256u32 {
            let from = SocketAddr::from(([10, 1, (n >> 8) as u8, n as u8], 4000));
            ids.push(s.receive(0, from, &join(PROTOCOL_VERSION)));
        }
        assert_eq!(ids[254], Some(255));
        assert_eq!(ids[255], None);
        assert_eq!(s.net_stats().get(SERVER_FULL), 1);
        assert_eq!(s.peers().count(), 255);
    }

    #[test]
    fn accepted_input_waits_for_end_tick() {
        let mut s = joined_pair();
        s.begin_tick();
        s.receive(1, addr(1), &input(1, Vec2::new(1.0, 0.0), Vec2::new(1.0, 0.0), false));
        assert_eq!(s.sim().player(1).unwrap().pos, Vec2::ZERO); // not yet
        let out = s.end_tick(1);
        assert_eq!(out.steps, vec![(1, MOVE_SPEED)]);
        assert_eq!(s.sim().player(1).unwrap().pos, Vec2::new(MOVE_SPEED, 0.0));
        // the queue drained: a second end_tick does nothing
        assert_eq!(s.end_tick(1), TickOutcome::default());
    }

    /// Ordering: shots resolve before anyone moves. Player 2 steps out of the
    /// line of fire in the same tick player 1 shoots — and is still hit,
    /// because the shot is judged against the world player 1 was shown.
    #[test]
    fn shots_resolve_before_moves() {
        let mut s = joined_pair();
        s.begin_tick();
        s.receive(1, addr(2), &input(1, Vec2::new(0.0, 1.0), Vec2::new(-1.0, 0.0), false));
        s.receive(1, addr(1), &input(1, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
        s.end_tick(1);
        assert_eq!(s.sim().player(2).unwrap().health, MAX_HEALTH - SHOT_DAMAGE);
        assert_eq!(s.sim().player(2).unwrap().pos, Vec2::new(10.0, MOVE_SPEED));
        let shot = s.telemetry().records().iter().find(|r| matches!(r.outcome, Outcome::Shot { .. })).unwrap();
        assert_eq!((shot.player, &shot.outcome), (1, &Outcome::Shot { hit: true, aim_err: 0.0 }));
    }

    #[test]
    fn kill_is_reported_with_the_killer() {
        let mut s = joined_pair();
        let mut kills = Vec::new();
        for t in 1..=(MAX_HEALTH / SHOT_DAMAGE) as u32 {
            s.begin_tick();
            s.receive(t, addr(1), &input(t, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
            kills.extend(s.end_tick(t).kills);
        }
        assert_eq!(kills, vec![1]);
        assert!(!s.sim().player(2).unwrap().alive);
    }

    #[test]
    fn begin_tick_respawns_before_the_snapshot() {
        let mut s = joined_pair();
        for t in 1..=(MAX_HEALTH / SHOT_DAMAGE) as u32 {
            s.begin_tick();
            s.receive(t, addr(1), &input(t, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
            s.end_tick(t);
        }
        let mut snap = Vec::new();
        for _ in 0..crate::sim::RESPAWN_TICKS {
            snap = s.begin_tick();
        }
        let p2 = snap.iter().find(|p| p.id == 2).unwrap();
        assert!(p2.alive && p2.health == MAX_HEALTH && p2.pos == Vec2::new(10.0, 0.0));
    }
}
