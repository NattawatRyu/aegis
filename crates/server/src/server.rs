//! The server's tick: everything between "a datagram arrived" and "the world
//! moved". Transport-agnostic — whoever owns the socket (the UDP loop in
//! [`crate::net`], or the in-process harness with made-up addresses) calls,
//! per tick:
//!
//! ```text
//! begin_tick  -> idle sessions end, respawns, then the snapshot clients act on
//! receive     -> once per datagram:
//!                address + token?  yes -> player budget -> decode -> pipeline
//!                                  no  -> IP budget -> decode -> Join only
//!                                         (no cookie -> Challenge; cookie -> admit)
//! end_tick    -> shots against the pre-move world, then moves
//! ```
//!
//! Identity is the source address **and** the session token issued to it. A
//! player id is issued only when a legal Join from a new address brings back
//! the cookie it was challenged with — proof it receives there;
//! everything that is not authenticated is counted in [`NetStats`] and
//! dropped, never written on a player's record — so a forger cannot frame its
//! victim. Both transports run this same code, so an in-process run and a UDP
//! run of the same scenario are directly comparable.
//!
//! Every verdict and every aim-evidence shot of an authenticated player is
//! written to [`Telemetry`]: that stream is what the detector (pillar C) reads.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use aegis_protocol::{split_frame, ClientMsg, LinkKey, PlayerId, PlayerState, ServerMsg, Vec2, TICK_HZ};
use aegis_telemetry::Telemetry;

use crate::guards::cookie::CookieJar;
use crate::guards::session::{self, Session};
use crate::guards::source_rate::SourceRate;
use crate::guards::{ip_sessions, joined, packet, version};
use crate::{ClientInput, GuardCtx, GuardVerdict, Pipeline, RejectReason, Sim, Wall};

/// [`NetStats`] label for a legal join refused because all 255 ids are taken.
pub const SERVER_FULL: &str = "server_full";

/// A session with no authenticated datagram for longer than this (5 s) is
/// ended: removed from the world, its id freed, a `Left` record written. A
/// real client sends every tick; one that stops has gone, or never played.
pub const IDLE_TICKS: u32 = 5 * TICK_HZ;

/// What one tick did, for callers that measure the world (the lab's kill
/// count and movement-authority check). The server itself keeps no metrics.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TickOutcome {
    /// One entry per kill, naming the killer.
    pub kills: Vec<PlayerId>,
    /// One entry per applied move: who, and how far they actually travelled.
    pub steps: Vec<(PlayerId, f32)>,
}

/// Datagrams dropped without a player to pin them on: rate-budget drops
/// (counted before anything is decoded) and everything unauthenticated —
/// from an address that never joined, or with the wrong token. Counters only —
/// bounded no matter how hard the server is flooded.
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
    /// Budget for authenticated datagrams, per player.
    player_rate: SourceRate<PlayerId>,
    /// Budget for everything else, per source IP.
    ip_rate: SourceRate<IpAddr>,
    /// Admitted address -> its session.
    sessions: BTreeMap<SocketAddr, Session>,
    /// Admitted address -> tick of its last authenticated datagram (or join).
    last_seen: BTreeMap<SocketAddr, u32>,
    /// The id issued last. The next goes to the first free id after it,
    /// wrapping — so a freed id is reused as late as possible.
    last_id: PlayerId,
    /// Issues and checks join cookies (return routability).
    cookies: CookieJar,
    /// What session tokens are MAC'd under: random, or the link key shared
    /// with a relay so it can pre-check them ([`Server::share_token_key`]).
    token_key: LinkKey,
    /// Player `id` enters at `spawns[(id - 1) % len]`.
    spawns: Vec<Vec2>,
    /// Inputs accepted this tick, in arrival order, waiting for `end_tick`.
    pending: Vec<(PlayerId, ClientInput)>,
}

/// What [`Server::receive`] asks the transport to send back to the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Send `ServerMsg::Challenge { cookie }`.
    Challenge(u64),
    /// Send `ServerMsg::Joined` with this session.
    Joined(Session),
}

impl Reply {
    /// The message on the wire, stamped with the current tick.
    pub fn to_msg(self, tick: u32) -> ServerMsg {
        match self {
            Reply::Challenge(cookie) => ServerMsg::Challenge { cookie },
            Reply::Joined(s) => ServerMsg::Joined { player_id: s.player_id, token: s.token, tick },
        }
    }
}

impl Server {
    /// A server whose players enter at `spawns`, in join order (wrapping), in
    /// an open arena.
    pub fn new(spawns: Vec<Vec2>) -> Self {
        Self::with_walls(spawns, &[])
    }

    /// Same, in an arena with `walls`.
    pub fn with_walls(spawns: Vec<Vec2>, walls: &[Wall]) -> Self {
        assert!(!spawns.is_empty(), "a server needs at least one spawn point");
        Self {
            sim: Sim::with_walls(walls),
            pipe: Pipeline::standard(),
            tel: Telemetry::new(),
            net: NetStats::default(),
            player_rate: SourceRate::new(),
            ip_rate: SourceRate::new(),
            sessions: BTreeMap::new(),
            last_seen: BTreeMap::new(),
            last_id: 0,
            cookies: CookieJar::new(),
            token_key: {
                let mut k = [0u8; 16];
                getrandom::fill(&mut k).expect("aegis-server: OS random source unavailable");
                k
            },
            spawns,
            pending: Vec::new(),
        }
    }

    /// MAC tokens issued from now on under `key` — the link key a relay in
    /// front holds, so it can drop a forged or guessed token at the edge.
    /// Set it before anyone joins: a token issued earlier fails the relay's
    /// check and its player is cut off there.
    pub fn share_token_key(&mut self, key: LinkKey) {
        self.token_key = key;
    }

    /// Start tick `tick`: end idle sessions, advance respawn timers, then
    /// return the whole world clients choose this tick's input in. Not what
    /// they are sent: each gets its player's [`Sim::view`] of it.
    pub fn begin_tick(&mut self, tick: u32) -> Vec<PlayerState> {
        let idle: Vec<SocketAddr> = self
            .last_seen
            .iter()
            .filter(|&(_, &seen)| tick.saturating_sub(seen) > IDLE_TICKS)
            .map(|(&a, _)| a)
            .collect();
        for a in idle {
            self.last_seen.remove(&a);
            if let Some(s) = self.sessions.remove(&a) {
                self.sim.despawn(s.player_id);
                self.pipe.forget(s.player_id);
                self.pending.retain(|&(p, _)| p != s.player_id);
                self.tel.left(tick, s.player_id);
            }
        }
        self.sim.step_respawns();
        self.sim.snapshot()
    }

    /// Receive one datagram (a token-framed `ClientMsg`) from `from`. An input
    /// that passes every guard is queued for `end_tick`; everything else is
    /// recorded or counted, and dropped.
    ///
    /// Returns what the transport must send back to `from`, if anything:
    /// a [`Reply::Challenge`] for a Join without a valid cookie, or
    /// [`Reply::Joined`] for one with — a new player, or a repeat from an
    /// admitted address (its last answer was lost; it only ever goes to the
    /// admitted address itself). Neither is larger than the Join that asked.
    pub fn receive(&mut self, tick: u32, from: SocketAddr, bytes: &[u8]) -> Option<Reply> {
        let framed = split_frame(bytes);
        let known = joined::check_source(&self.sessions, from);

        // Authenticated: this address and its token. Its own budget, so
        // forgeries from the same IP cannot spend it.
        if let (Some((token, body)), Ok(s)) = (framed, known) {
            if session::verify(&s, token).is_ok() {
                self.last_seen.insert(from, tick);
                if let Err(r) = self.player_rate.check(tick, s.player_id) {
                    self.net.drop(r.label());
                    return None;
                }
                return self.receive_from(tick, s, body).map(Reply::Joined);
            }
        }

        // Everything else shares its IP's budget, and only a Join comes of it.
        if let Err(r) = self.ip_rate.check(tick, from.ip()) {
            self.net.drop(r.label());
            return None;
        }
        let msg = match framed.ok_or(RejectReason::MalformedPacket).and_then(|(_, body)| packet::decode_client(body)) {
            Ok(m) => m,
            Err(r) => {
                self.net.drop(r.label());
                return None;
            }
        };
        let refused = match (msg, known) {
            (ClientMsg::Join { protocol, cookie, .. }, known) => match (version::check_join(protocol), known, cookie) {
                (Err(r), _, _) => r,
                (Ok(()), Ok(s), _) => return Some(Reply::Joined(s)), // lost Joined: answer again
                // Unproven address: a cookie to bring back, and nothing else.
                (Ok(()), Err(_), None) => return Some(Reply::Challenge(self.cookies.issue(from, tick))),
                (Ok(()), Err(_), Some(c)) => match self.cookies.verify(from, tick, c) {
                    Ok(()) => return self.admit(tick, from).map(Reply::Joined),
                    Err(r) => r,
                },
            },
            (ClientMsg::Input { .. }, Ok(_)) => RejectReason::BadToken,
            (ClientMsg::Input { .. }, Err(not_joined)) => not_joined,
        };
        self.net.drop(refused.label());
        None
    }

    /// A legal Join with a valid cookie from an address with no session:
    /// create the player.
    fn admit(&mut self, tick: u32, from: SocketAddr) -> Option<Session> {
        if let Err(r) = ip_sessions::check_join(&self.sessions, from.ip()) {
            self.net.drop(r.label());
            return None;
        }
        let Some(player_id) = self.next_free_id() else {
            self.net.drop(SERVER_FULL);
            return None;
        };
        self.last_id = player_id;
        let s = Session { player_id, token: session::new_token(&self.token_key, from) };
        self.sessions.insert(from, s);
        self.last_seen.insert(from, tick);
        self.sim.spawn(player_id, self.spawns[(player_id as usize - 1) % self.spawns.len()]);
        Some(s)
    }

    /// The first id in 1..=255 after `last_id` (wrapping) that nobody holds.
    fn next_free_id(&self) -> Option<PlayerId> {
        let taken: std::collections::BTreeSet<PlayerId> = self.sessions.values().map(|s| s.player_id).collect();
        (1..=PlayerId::MAX)
            .map(|k| ((self.last_id as u16 + k as u16 - 1) % PlayerId::MAX as u16 + 1) as PlayerId)
            .find(|id| !taken.contains(id))
    }

    /// An authenticated datagram: everything is on the player's record.
    fn receive_from(&mut self, tick: u32, s: Session, body: &[u8]) -> Option<Session> {
        let player = s.player_id;
        let msg = match packet::decode_client(body) {
            Ok(m) => m,
            Err(r) => {
                self.tel.reject(tick, player, r.label());
                return None;
            }
        };
        match msg {
            // Already in: a repeated join changes nothing but is answered again.
            ClientMsg::Join { protocol, .. } => match version::check_join(protocol) {
                Ok(()) => Some(s),
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
        self.sessions.get(&from).map(|s| s.player_id)
    }

    /// Every admitted address and its player — who a snapshot goes to.
    pub fn peers(&self) -> impl Iterator<Item = (SocketAddr, PlayerId)> + '_ {
        self.sessions.iter().map(|(&a, s)| (a, s.player_id))
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

    /// `n` drops at once (see [`crate::net::BACKLOG_FULL`]).
    pub(crate) fn count_drops(&mut self, label: &'static str, n: u64) {
        if n > 0 {
            *self.net.dropped.entry(label).or_insert(0) += n;
        }
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
    use aegis_protocol::{frame, NO_TOKEN, PROTOCOL_VERSION};
    use aegis_telemetry::Outcome;

    fn addr(n: u8) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, n], 4000))
    }

    fn join(protocol: u16) -> Vec<u8> {
        join_with(protocol, None)
    }

    fn join_with(protocol: u16, cookie: Option<u64>) -> Vec<u8> {
        frame(NO_TOKEN, &ClientMsg::Join { name: "t".into(), protocol, cookie })
    }

    /// The full handshake a real client does: Join, and if challenged, Join
    /// again with the cookie. The session, if admitted.
    fn hs(s: &mut Server, tick: u32, from: SocketAddr) -> Option<Session> {
        match s.receive(tick, from, &join(PROTOCOL_VERSION))? {
            Reply::Joined(x) => Some(x),
            Reply::Challenge(c) => match s.receive(tick, from, &join_with(PROTOCOL_VERSION, Some(c)))? {
                Reply::Joined(x) => Some(x),
                Reply::Challenge(_) => panic!("challenged twice"),
            },
        }
    }

    fn input(token: u64, seq: u32, move_dir: Vec2, aim: Vec2, shoot: bool) -> Vec<u8> {
        frame(token, &ClientMsg::Input { seq, tick: seq, move_dir, aim, shoot })
    }

    fn walk(token: u64, seq: u32) -> Vec<u8> {
        input(token, seq, Vec2::new(1.0, 0.0), Vec2::new(1.0, 0.0), false)
    }

    fn server() -> Server {
        Server::new(vec![Vec2::ZERO, Vec2::new(10.0, 0.0)])
    }

    fn admit(s: &mut Server, from: SocketAddr) -> u64 {
        hs(s, 0, from).expect("legal join refused").token
    }

    /// Player 1 at the origin, player 2 at (10, 0), and their tokens.
    fn joined_pair() -> (Server, u64, u64) {
        let mut s = server();
        let t1 = admit(&mut s, addr(1));
        let t2 = admit(&mut s, addr(2));
        (s, t1, t2)
    }

    #[test]
    fn legal_join_spawns_once_and_is_not_recorded() {
        let mut s = server();
        let first = hs(&mut s, 0, addr(1)).unwrap();
        // a repeat is answered again, same session (the reply may have been lost)
        assert_eq!(hs(&mut s, 0, addr(1)), Some(first));
        assert_eq!(first.player_id, 1);
        assert_eq!(s.sim().snapshot().len(), 1);
        assert!(s.telemetry().is_empty());
    }

    #[test]
    fn every_player_gets_its_own_token() {
        let (_, t1, t2) = joined_pair();
        assert_ne!(t1, t2);
        assert!(t1 != NO_TOKEN && t2 != NO_TOKEN);
    }

    #[test]
    fn ids_follow_legal_join_order_and_spawns_wrap() {
        let mut s = server();
        let id = |s: &mut Server, n| hs(s, 0, addr(n)).map(|x| x.player_id);
        assert_eq!(id(&mut s, 1), Some(1));
        assert_eq!(s.receive(0, addr(2), &join(PROTOCOL_VERSION + 1)), None); // refused: no id spent
        assert_eq!(id(&mut s, 3), Some(2));
        assert_eq!(id(&mut s, 4), Some(3));
        assert_eq!(s.sim().player(2).unwrap().pos, Vec2::new(10.0, 0.0));
        assert_eq!(s.sim().player(3).unwrap().pos, Vec2::ZERO);
    }

    /// Return routability: a first Join gets a challenge and nothing else —
    /// no id, no spawn, no snapshot target. Only the cookie, brought back
    /// from the same address, admits.
    #[test]
    fn first_join_is_only_challenged() {
        let mut s = server();
        let Some(Reply::Challenge(c)) = s.receive(0, addr(1), &join(PROTOCOL_VERSION)) else { panic!("not challenged") };
        assert_eq!(s.peers().count(), 0);
        assert!(s.sim().snapshot().is_empty());
        // the cookie from another address, or a wrong one, admits nobody
        assert_eq!(s.receive(0, addr(2), &join_with(PROTOCOL_VERSION, Some(c))), None);
        assert_eq!(s.receive(0, addr(1), &join_with(PROTOCOL_VERSION, Some(c ^ 1))), None);
        assert_eq!(s.net_stats().get("bad_cookie"), 2);
        assert_eq!(s.peers().count(), 0);
        let Some(Reply::Joined(x)) = s.receive(0, addr(1), &join_with(PROTOCOL_VERSION, Some(c))) else { panic!("not admitted") };
        assert_eq!(x.player_id, 1);
    }

    /// The cookie's edge, through the server: issued at tick 0 it still
    /// admits at the last tick of the next bucket, not one tick later.
    #[test]
    fn a_stale_cookie_is_refused() {
        use crate::guards::cookie::BUCKET_TICKS;
        let mut s = server();
        let Some(Reply::Challenge(c)) = s.receive(0, addr(1), &join(PROTOCOL_VERSION)) else { panic!() };
        assert_eq!(s.receive(2 * BUCKET_TICKS, addr(1), &join_with(PROTOCOL_VERSION, Some(c))), None);
        assert_eq!(s.net_stats().get("bad_cookie"), 1);
        assert!(matches!(s.receive(2 * BUCKET_TICKS - 1, addr(1), &join_with(PROTOCOL_VERSION, Some(c))), Some(Reply::Joined(_))));
    }

    /// A Join with a cookie but the wrong version is a version refusal — the
    /// cookie is never even looked at.
    #[test]
    fn version_is_checked_before_the_cookie() {
        let mut s = server();
        assert_eq!(s.receive(0, addr(1), &join_with(PROTOCOL_VERSION + 1, Some(123))), None);
        assert_eq!((s.net_stats().get("bad_version"), s.net_stats().get("bad_cookie")), (1, 0));
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
        s.receive(1, addr(1), &walk(NO_TOKEN, 1));
        s.receive(1, addr(1), &[0xFF, 0xFF, 0xFF]); // too short to carry a token
        s.receive(1, addr(1), &[0xFF; 12]); // a token, then garbage
        assert_eq!(s.end_tick(1), TickOutcome::default());
        assert_eq!(s.peers().count(), 0);
        assert!(s.telemetry().is_empty());
        assert_eq!((s.net_stats().get("not_joined"), s.net_stats().get("malformed_packet")), (1, 2));
    }

    #[test]
    fn same_ip_other_port_is_not_the_player_even_with_its_token() {
        let (mut s, t1, _) = joined_pair();
        let other = SocketAddr::from(([10, 0, 0, 1], 4001));
        s.receive(1, other, &walk(t1, 1));
        assert_eq!(s.end_tick(1), TickOutcome::default());
        assert_eq!(s.net_stats().get("not_joined"), 1);
    }

    /// The forgery: the victim's address with any token but the victim's —
    /// none, a guess, or the forger's own valid one. Nothing moves, nothing
    /// lands on the victim's record, each one is counted.
    #[test]
    fn forged_source_with_wrong_token_is_counted_not_applied() {
        let (mut s, t1, t2) = joined_pair();
        for (seq, t) in [NO_TOKEN, t1 ^ 1, t2].into_iter().enumerate() {
            s.receive(1, addr(1), &walk(t, seq as u32 + 1));
        }
        assert_eq!(s.end_tick(1), TickOutcome::default());
        assert!(s.telemetry().is_empty(), "victim's record: {:?}", s.telemetry().records());
        assert_eq!(s.net_stats().get("bad_token"), 3);
    }

    /// Starvation: a burst of forgeries from the victim's IP arrives first and
    /// fills that IP's budget. The victim's real, token-bearing input still
    /// gets in — it is budgeted per player, not per IP.
    #[test]
    fn forged_burst_does_not_starve_the_victim() {
        let (mut s, t1, _) = joined_pair();
        for seq in 0..MAX_PER_TICK * 4 {
            s.receive(1, addr(1), &walk(NO_TOKEN, 1_000 + seq));
        }
        s.receive(1, addr(1), &walk(t1, 1));
        assert_eq!(s.end_tick(1).steps, vec![(1, MOVE_SPEED)]);
        assert_eq!(s.telemetry().per_player(1).accepted, 1);
        assert_eq!(s.net_stats().get("bad_token"), MAX_PER_TICK as u64);
        assert_eq!(s.net_stats().get("source_rate"), MAX_PER_TICK as u64 * 3);
    }

    /// A lost Joined is recovered by joining again — from the admitted
    /// address, the session comes back unchanged; from anywhere else it is a
    /// new player, never the old one's token.
    #[test]
    fn rejoin_returns_the_same_session_only_to_its_address() {
        let (mut s, t1, _) = joined_pair();
        assert_eq!(hs(&mut s, 0, addr(1)), Some(Session { player_id: 1, token: t1 }));
        let other = hs(&mut s, 0, addr(9)).unwrap();
        assert_ne!(other.token, t1);
        assert_eq!(other.player_id, 3);
    }

    /// The timeout edge: idle for exactly IDLE_TICKS is still in; one more and
    /// the session ends — gone from the world, `Left` on the record, its
    /// token worthless.
    #[test]
    fn idle_session_ends_one_tick_past_the_timeout() {
        let mut s = server();
        let t1 = admit(&mut s, addr(1)); // last seen: tick 0
        assert_eq!(s.begin_tick(IDLE_TICKS).len(), 1);
        assert_eq!(s.player_id(addr(1)), Some(1));
        assert!(s.begin_tick(IDLE_TICKS + 1).is_empty());
        assert_eq!(s.player_id(addr(1)), None);
        let last = s.telemetry().records().last().unwrap();
        assert_eq!((last.tick, last.player, &last.outcome), (IDLE_TICKS + 1, 1, &Outcome::Left));
        s.receive(IDLE_TICKS + 1, addr(1), &walk(t1, 1));
        assert_eq!(s.net_stats().get("not_joined"), 1);
    }

    #[test]
    fn authenticated_traffic_keeps_a_session_alive_forgeries_do_not() {
        let (mut s, t1, _) = joined_pair();
        s.receive(100, addr(1), &walk(t1, 1)); // player 1: real input at 100
        for t in 1..=IDLE_TICKS + 1 {
            s.receive(t, addr(2), &walk(NO_TOKEN, t)); // player 2: only forgeries
        }
        s.begin_tick(IDLE_TICKS + 1);
        assert_eq!(s.player_id(addr(1)), Some(1));
        assert_eq!(s.player_id(addr(2)), None);
    }

    /// One IP, many ports: MAX_PER_IP sessions, then refusals; another IP is
    /// unaffected; when one of the IP's sessions ends, it may join again.
    #[test]
    fn sessions_per_ip_are_capped_and_freed_by_the_timeout() {
        use crate::guards::ip_sessions::MAX_PER_IP;
        let mut s = server();
        let port = |p: u16| SocketAddr::from(([10, 0, 0, 1], 5000 + p));
        // One join per tick: a handshake is two datagrams against the IP budget.
        for p in 0..MAX_PER_IP as u16 {
            assert!(hs(&mut s, p as u32, port(p)).is_some(), "port {p}");
        }
        assert_eq!(hs(&mut s, 10, port(99)), None);
        assert_eq!(s.net_stats().get("ip_sessions"), 1);
        assert!(hs(&mut s, 10, addr(2)).is_some());

        s.begin_tick(IDLE_TICKS + 1); // port 0 idle since tick 0: its slot frees
        assert!(hs(&mut s, IDLE_TICKS + 1, port(99)).is_some());
    }

    /// Ids are handed out round-robin, so a freed id is the last one reused.
    #[test]
    fn a_freed_id_is_not_the_next_one_issued() {
        let mut s = server();
        admit(&mut s, addr(1));
        s.begin_tick(IDLE_TICKS + 1); // id 1 ends
        let ids: Vec<_> = (2..=3).map(|n| hs(&mut s, IDLE_TICKS + 1, addr(n)).unwrap().player_id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    /// When an id does come round again, its new owner starts clean: the old
    /// owner's seq 500 does not make the newcomer's seq 1 a "replay", and it
    /// enters at its spawn, alive.
    #[test]
    fn a_reused_id_inherits_nothing() {
        let mut s = server();
        let old = admit(&mut s, SocketAddr::from(([10, 2, 0, 1], 4000)));
        s.receive(1, SocketAddr::from(([10, 2, 0, 1], 4000)), &walk(old, 500));
        for n in 2..=255u8 {
            let from = SocketAddr::from(([10, 3, 0, n], 4000));
            assert_eq!(hs(&mut s, 100, from).map(|x| x.player_id), Some(n));
        }
        s.begin_tick(IDLE_TICKS + 2); // only id 1 is idle long enough
        let newcomer = SocketAddr::from(([10, 4, 0, 1], 4000));
        let fresh = hs(&mut s, IDLE_TICKS + 2, newcomer).unwrap();
        assert_eq!(fresh.player_id, 1);
        assert_eq!(s.sim().player(1).unwrap().pos, Vec2::ZERO);
        s.receive(IDLE_TICKS + 3, newcomer, &walk(fresh.token, 1));
        let out = s.end_tick(IDLE_TICKS + 3);
        assert_eq!(out.steps, vec![(1, MOVE_SPEED)], "net: {:?}", s.net_stats());
    }

    #[test]
    fn garbage_from_a_player_is_on_their_record() {
        let (mut s, _, t2) = joined_pair();
        let mut bad = t2.to_le_bytes().to_vec();
        bad.extend([0xFF, 0xFF, 0xFF]);
        s.receive(1, addr(2), &bad);
        let r = &s.telemetry().records()[0];
        assert_eq!((r.player, &r.outcome), (2, &Outcome::Rejected { reason: "malformed_packet" }));
    }

    /// The edge: datagram MAX_PER_TICK from one player is decoded, the next is
    /// not — it never reaches the pipeline, so input_rate never sees it and
    /// there is no record of it, only a counter.
    #[test]
    fn source_rate_drops_past_the_cap_before_decode() {
        let (mut s, t1, _) = joined_pair();
        for seq in 1..=MAX_PER_TICK + 1 {
            s.receive(1, addr(1), &input(t1, seq, Vec2::ZERO, Vec2::ZERO, false));
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
            s.receive(1, addr(9), &walk(NO_TOKEN, 1));
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
            ids.push(hs(&mut s, 0, from).map(|x| x.player_id));
        }
        assert_eq!(ids[254], Some(255));
        assert_eq!(ids[255], None);
        assert_eq!(s.net_stats().get(SERVER_FULL), 1);
        assert_eq!(s.peers().count(), 255);
    }

    #[test]
    fn accepted_input_waits_for_end_tick() {
        let (mut s, t1, _) = joined_pair();
        s.begin_tick(1);
        s.receive(1, addr(1), &walk(t1, 1));
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
        let (mut s, t1, t2) = joined_pair();
        s.begin_tick(1);
        s.receive(1, addr(2), &input(t2, 1, Vec2::new(0.0, 1.0), Vec2::new(-1.0, 0.0), false));
        s.receive(1, addr(1), &input(t1, 1, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
        s.end_tick(1);
        assert_eq!(s.sim().player(2).unwrap().health, MAX_HEALTH - SHOT_DAMAGE);
        assert_eq!(s.sim().player(2).unwrap().pos, Vec2::new(10.0, MOVE_SPEED));
        let shot = s.telemetry().records().iter().find(|r| matches!(r.outcome, Outcome::Shot { .. })).unwrap();
        assert_eq!((shot.player, &shot.outcome), (1, &Outcome::Shot { hit: true, aim_err: 0.0 }));
    }

    #[test]
    fn kill_is_reported_with_the_killer() {
        let (mut s, t1, _) = joined_pair();
        let mut kills = Vec::new();
        for t in 1..=(MAX_HEALTH / SHOT_DAMAGE) as u32 {
            s.begin_tick(t);
            s.receive(t, addr(1), &input(t1, t, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
            kills.extend(s.end_tick(t).kills);
        }
        assert_eq!(kills, vec![1]);
        assert!(!s.sim().player(2).unwrap().alive);
    }

    #[test]
    fn begin_tick_respawns_before_the_snapshot() {
        let (mut s, t1, _) = joined_pair();
        for t in 1..=(MAX_HEALTH / SHOT_DAMAGE) as u32 {
            s.begin_tick(t);
            s.receive(t, addr(1), &input(t1, t, Vec2::ZERO, Vec2::new(1.0, 0.0), true));
            s.end_tick(t);
        }
        let mut snap = Vec::new();
        let dead_at = (MAX_HEALTH / SHOT_DAMAGE) as u32;
        for t in dead_at + 1..=dead_at + crate::sim::RESPAWN_TICKS {
            snap = s.begin_tick(t);
        }
        let p2 = snap.iter().find(|p| p.id == 2).unwrap();
        assert!(p2.alive && p2.health == MAX_HEALTH && p2.pos == Vec2::new(10.0, 0.0));
    }
}
