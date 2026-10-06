//! The UDP transport: a [`Server`] behind a `std::net::UdpSocket`. No async
//! runtime — one socket, one thread, a fixed 30Hz tick.
//!
//! This file owns bytes on the wire and nothing else. Who a datagram is from,
//! whether it gets decoded and what it does to the world are all decided in
//! [`Server::receive`], so this loop and the in-process harness run the same
//! defenses.
//!
//! A tick, as [`NetServer::run_tick`] drives it:
//!
//! ```text
//! begin_tick  -> Snapshot to every admitted address
//! recv_one    -> repeated until the tick's deadline; a legal Join is
//!                answered with Joined
//! end_tick    -> fold the accepted inputs into the world
//! ```
//!
//! The three steps are public so a test can drive the loop in lockstep (send,
//! then read exactly what was sent) instead of against the clock.
//!
//! Source addresses can be forged; that is answered in [`Server::receive`] by
//! the session token (see [`crate::guards::session`]), not here.
//!
//! Behind a relay ([`NetServer::behind_relay`], the `aegis-relay` crate) this
//! socket is the origin: it hears only the relay, reads each datagram's real
//! client address out of its envelope, and answers through the relay.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::collections::HashMap;

use aegis_protocol::{encode, unwrap, wrap, Dir, EnvelopeError, LinkKey, ServerMsg, Sid, ENVELOPE_MAX, TICK_HZ};

use crate::{Reply, Server, TickOutcome};

/// One tick of wall-clock time at [`TICK_HZ`].
pub const TICK: Duration = Duration::from_nanos(1_000_000_000 / TICK_HZ as u64);

pub use aegis_protocol::MAX_DATAGRAM;

/// [`crate::NetStats`] label for a datagram larger than [`MAX_DATAGRAM`]
/// (Windows reports these as an error instead of truncating them).
pub const OVERSIZE: &str = "oversize";

/// [`crate::NetStats`] label, behind a relay: a datagram from any address
/// but the relay's. Someone found the origin and is talking to it directly.
pub const NOT_RELAY: &str = "not_relay";

/// [`crate::NetStats`] label, behind a relay: an authentic envelope that is
/// not well-formed, or a datagram too short to carry a MAC.
pub const BAD_ENVELOPE: &str = "bad_envelope";

/// [`crate::NetStats`] label, behind a relay: a datagram from the relay's
/// address whose MAC is wrong — someone forging the relay's source address
/// without the link key.
pub const BAD_MAC: &str = "bad_mac";

/// [`crate::NetStats`] label, behind a relay: a datagram from an admitted
/// player's address under a session id that is not the one it was admitted
/// under. Someone holding keys of their own, forging that player's address:
/// were it read, a re-Join would move the player's replies to the forger's
/// keys.
pub const SID_MISMATCH: &str = "sid_mismatch";

/// [`crate::NetStats`] label: a datagram read off the socket while
/// [`BACKLOG`] were already waiting for the tick loop, dropped.
pub const BACKLOG_FULL: &str = "backlog_full";

/// Most datagrams that may wait between the reader and the tick loop. A
/// flood faster than the loop can judge is dropped here, at a fixed memory
/// cost, as the kernel's own socket buffer would — never queued without
/// bound. Several ticks of a full lobby's traffic.
pub const BACKLOG: usize = 4096;

/// WSAEMSGSIZE: the datagram was larger than the buffer, and is gone.
const WSAEMSGSIZE: i32 = 10040;

/// What the reader thread hands the tick loop.
enum Rx {
    Datagram(SocketAddr, Vec<u8>),
    Oversize,
}

/// The socket's one reader: a thread in a blocking `recv_from` with **no
/// read timeout**, passing every datagram, in order, down a channel. The
/// tick loop waits on the channel, whose timeout is safe. A read timeout on
/// the socket itself is suspect on Windows: the relay lost datagrams with a
/// 10 ms poll (4 of 12 parallel runs, 0 of 12 after going blocking), and the
/// old read-to-deadline loop of this file, replayed on a bare socket under
/// load, lost 1 datagram in 20000 in 4 of 35 runs against 0 of 35 for this
/// reader (2026-10-02). Suggestive, not proof of mechanism — part may be the
/// socket buffer overflowing while the tick loop is busy, which a reader that
/// never stops draining also helps. Guarded by
/// `real_time_ticks_read_every_datagram`.
struct Reader {
    rx: Receiver<io::Result<Rx>>,
    /// Datagrams dropped because the backlog was full, not yet counted.
    full: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    wake: SocketAddr,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// A reader holding at most `backlog` datagrams for the tick loop.
    fn spawn(sock: UdpSocket, backlog: usize) -> io::Result<Self> {
        let local = sock.local_addr()?;
        let wake = match local.ip() {
            ip if ip.is_unspecified() && local.is_ipv4() => SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local.port()),
            ip if ip.is_unspecified() => SocketAddr::new(Ipv6Addr::LOCALHOST.into(), local.port()),
            _ => local,
        };
        let (tx, rx) = mpsc::sync_channel(backlog);
        let full = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (f, flag) = (full.clone(), stop.clone());
        let thread = std::thread::spawn(move || read_loop(&sock, &tx, &f, &flag));
        Ok(Self { rx, full, stop, wake, thread: Some(thread) })
    }
}

impl Drop for Reader {
    /// Unblock the read with one empty datagram; the thread checks the flag
    /// before handling anything it reads.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let local: SocketAddr =
            if self.wake.is_ipv4() { (Ipv4Addr::LOCALHOST, 0).into() } else { (Ipv6Addr::LOCALHOST, 0).into() };
        if let Ok(w) = UdpSocket::bind(local) {
            let _ = w.send_to(&[], self.wake);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_loop(sock: &UdpSocket, tx: &SyncSender<io::Result<Rx>>, full: &AtomicU64, stop: &AtomicBool) {
    let mut buf = vec![0u8; MAX_DATAGRAM + ENVELOPE_MAX];
    loop {
        let got = sock.recv_from(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let item = match got {
            Ok((n, peer)) => Ok(Rx::Datagram(peer, buf[..n].to_vec())),
            Err(e) if e.raw_os_error() == Some(WSAEMSGSIZE) => Ok(Rx::Oversize),
            // Windows: an earlier send went to a closed port. Says nothing
            // about this socket; one client leaving must not stop the server.
            Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
            Err(e) => Err(e),
        };
        if item.is_err() {
            // The socket is broken: hand the error over (waiting for room —
            // it is the last thing sent) and stop.
            let _ = tx.send(item);
            return;
        }
        match tx.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                full.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

pub struct NetServer {
    server: Server,
    sock: UdpSocket,
    reader: Reader,
    tick: u32,
    /// Replies sent, when a lockstep driver asked to see them.
    reply_log: Option<Vec<(SocketAddr, ServerMsg)>>,
    /// The relay this server is the origin behind, and the key they share.
    relay: Option<(SocketAddr, LinkKey)>,
    /// Behind a relay: the session id each admitted address was admitted
    /// under — whose keys the relay seals its replies with. Bound at the
    /// `Joined`, never moved while the player stays; dropped when it leaves.
    sids: HashMap<SocketAddr, Sid>,
}

impl NetServer {
    pub fn bind(addr: impl ToSocketAddrs, server: Server) -> io::Result<Self> {
        let sock = UdpSocket::bind(addr)?;
        let reader = Reader::spawn(sock.try_clone()?, BACKLOG)?;
        Ok(Self { server, sock, reader, tick: 0, reply_log: None, relay: None, sids: HashMap::new() })
    }

    /// Become the origin behind the relay whose upstream socket is `relay`,
    /// sharing `key` with it: from now on only datagrams from that address
    /// with a valid MAC are read (each a [`wrap`]ped client datagram, judged
    /// against the client's address), and everything sent goes to it
    /// wrapped. A client never receives a byte from this socket, so it never
    /// learns this address; and forging the relay's address is not enough to
    /// speak for a client without the key.
    ///
    /// Not a firewall: a datagram from elsewhere is dropped after it has
    /// crossed the link, so a flood at a leaked origin address still fills
    /// it. What protects the origin is that its address is never published.
    ///
    /// Tokens issued from now on are MAC'd under `key` too, so the relay can
    /// drop a forged token before it crosses ([`Server::share_token_key`]):
    /// call this before anyone joins. And the relay proves every Join's
    /// address with its own cookie before the Join crosses, so this server
    /// admits a Join with a cookie without re-checking it
    /// ([`Server::trust_edge_cookies`]) — safe only because nothing but the
    /// relay, under the key, is read.
    ///
    /// Each envelope also names the client's session id at the relay (whose
    /// keys seal its leg). A player is bound to the sid it was admitted
    /// under: from its address, any other sid is dropped ([`SID_MISMATCH`]),
    /// and its replies go out under its own. Without that, any player — who
    /// holds keys of their own — could forge a victim's address and re-Join
    /// in its name, moving the victim's replies, token and all, to keys the
    /// forger can open.
    pub fn behind_relay(&mut self, relay: SocketAddr, key: LinkKey) {
        self.relay = Some((relay, key));
        self.server.share_token_key(key);
        self.server.trust_edge_cookies();
    }

    /// Send `bytes` to client `to`: directly, or wrapped through the relay
    /// for the session `sid`. Behind a relay with no sid, nothing is sent:
    /// the relay could not seal it for anyone.
    fn send(&self, to: SocketAddr, sid: Option<&Sid>, bytes: &[u8]) {
        let _ = match (&self.relay, sid) {
            (Some((r, key)), Some(sid)) => self.sock.send_to(&wrap(key, Dir::Down, to, sid, bytes), r),
            (Some(_), None) => return,
            (None, _) => self.sock.send_to(bytes, to),
        };
    }

    /// For a lockstep driver (the harness): record every reply sent, so it
    /// knows which client sockets to read and what to expect there. Grows
    /// until [`take_replies`](Self::take_replies) — not for a live server.
    pub fn log_replies(&mut self) {
        self.reply_log = Some(Vec::new());
    }

    /// The replies sent since the last call, in send order.
    pub fn take_replies(&mut self) -> Vec<(SocketAddr, ServerMsg)> {
        self.reply_log.as_mut().map(std::mem::take).unwrap_or_default()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// The tick datagrams are currently being received into. 0 until the
    /// first `begin_tick`: that is when the joins arrive.
    pub fn tick(&self) -> u32 {
        self.tick
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    pub fn into_server(self) -> Server {
        self.server
    }

    /// Advance to the next tick and send every admitted address its player's
    /// view — never the whole world. A send that fails is that client's
    /// problem, not the tick's.
    pub fn begin_tick(&mut self) -> io::Result<()> {
        self.tick += 1;
        self.server.begin_tick(self.tick);
        // A player who left frees its address for a new session.
        let server = &self.server;
        self.sids.retain(|a, _| server.player_id(*a).is_some());
        for (to, id) in self.server.peers() {
            let snap = encode(&ServerMsg::Snapshot { tick: self.tick, players: self.server.sim().view(id) });
            self.send(to, self.sids.get(&to), &snap);
        }
        Ok(())
    }

    /// Wait up to `wait` for one datagram and feed it to the server. Returns
    /// whether a datagram was consumed. The wait is on the reader's channel,
    /// never a socket timeout (see [`Reader`]).
    pub fn recv_one(&mut self, wait: Duration) -> io::Result<bool> {
        let full = self.reader.full.swap(0, Ordering::Relaxed);
        self.server.count_drops(BACKLOG_FULL, full);
        let (peer, mut datagram) = match self.reader.rx.recv_timeout(wait) {
            Ok(Ok(Rx::Datagram(peer, d))) => (peer, d),
            Ok(Ok(Rx::Oversize)) => {
                self.server.count_drop(OVERSIZE);
                return Ok(true);
            }
            Ok(Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => return Err(io::Error::other("aegis-server: socket reader stopped")),
        };
        // Behind a relay: only the relay, and only an envelope.
        let (from, sid, bytes) = match &self.relay {
            None => (peer, None, &datagram[..]),
            Some((r, _)) if peer != *r => {
                self.server.count_drop(NOT_RELAY);
                return Ok(true);
            }
            Some((_, key)) => match unwrap(key, Dir::Up, &mut datagram) {
                Ok((from, sid, bytes)) => (from, Some(sid), bytes),
                Err(e) => {
                    self.server.count_drop(if e == EnvelopeError::BadMac { BAD_MAC } else { BAD_ENVELOPE });
                    return Ok(true);
                }
            },
        };
        // An admitted address speaks under the sid it was admitted under, or
        // not at all.
        if let (Some(sid), Some(bound)) = (sid, self.sids.get(&from)) {
            if sid != *bound {
                self.server.count_drop(SID_MISMATCH);
                return Ok(true);
            }
        }
        if let Some(reply) = self.server.receive(self.tick, from, bytes) {
            if let (Reply::Joined(_), Some(sid)) = (reply, sid) {
                self.sids.insert(from, sid);
            }
            let msg = reply.to_msg(self.tick);
            self.send(from, sid.as_ref(), &encode(&msg));
            if let Some(log) = &mut self.reply_log {
                log.push((from, msg));
            }
        }
        Ok(true)
    }

    /// Fold this tick's accepted inputs into the world.
    pub fn end_tick(&mut self) -> TickOutcome {
        self.server.end_tick(self.tick)
    }

    /// One real-time tick: snapshot out, read until the deadline, fold.
    pub fn run_tick(&mut self) -> io::Result<TickOutcome> {
        let deadline = Instant::now() + TICK;
        self.begin_tick()?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.recv_one(left)?;
        }
        Ok(self.end_tick())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{decode, frame, ClientMsg, Vec2, NO_TOKEN, PROTOCOL_VERSION};

    const WAIT: Duration = Duration::from_secs(1);

    fn net() -> NetServer {
        NetServer::bind("127.0.0.1:0", Server::new(vec![Vec2::ZERO, Vec2::new(10.0, 0.0)])).unwrap()
    }

    fn client() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(WAIT)).unwrap();
        s
    }

    fn join() -> Vec<u8> {
        join_with(None)
    }

    fn join_with(cookie: Option<u64>) -> Vec<u8> {
        frame(NO_TOKEN, &ClientMsg::Join { name: "t".into(), protocol: PROTOCOL_VERSION, cookie })
    }

    /// The client side of the handshake, over the wire: Join, read the
    /// challenge, Join with its cookie. Returns what `joined` returns.
    fn connect(n: &mut NetServer, c: &UdpSocket) -> ((u8, u32), u64) {
        let to = n.local_addr().unwrap();
        c.send_to(&join(), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        let cookie = match read(c) {
            ServerMsg::Challenge { cookie } => cookie,
            m => panic!("expected Challenge, got {m:?}"),
        };
        c.send_to(&join_with(Some(cookie)), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        joined(c)
    }

    fn input(token: u64, seq: u32) -> Vec<u8> {
        frame(
            token,
            &ClientMsg::Input { seq, tick: seq, move_dir: Vec2::new(1.0, 0.0), aim: Vec2::new(1.0, 0.0), shoot: false },
        )
    }

    fn read(c: &UdpSocket) -> ServerMsg {
        let mut buf = [0u8; MAX_DATAGRAM];
        let n = c.recv(&mut buf).expect("no reply from server");
        decode(&buf[..n]).unwrap()
    }

    /// (player_id, tick) of the `Joined` the client reads next, and its token.
    fn joined(c: &UdpSocket) -> ((u8, u32), u64) {
        match read(c) {
            ServerMsg::Joined { player_id, token, tick } => ((player_id, tick), token),
            m => panic!("expected Joined, got {m:?}"),
        }
    }

    #[test]
    fn join_is_answered_and_snapshots_follow() {
        let mut n = net();
        let c = client();
        let (who, token) = connect(&mut n, &c);
        assert_eq!(who, (1, 0));
        assert_ne!(token, NO_TOKEN);

        n.begin_tick().unwrap();
        match read(&c) {
            ServerMsg::Snapshot { tick: 1, players } => assert_eq!(players.len(), 1),
            m => panic!("expected the tick-1 snapshot, got {m:?}"),
        }
    }

    #[test]
    fn nothing_to_read_returns_false_on_time() {
        let mut n = net();
        let t = Instant::now();
        assert!(!n.recv_one(Duration::from_millis(20)).unwrap());
        assert!(t.elapsed() < WAIT);
    }

    /// The Windows edge: a snapshot sent to a client that has closed its
    /// socket makes the next `recv_from` fail with ConnectionReset. The
    /// server must skip it and still read the next real datagram.
    #[test]
    fn a_vanished_client_does_not_stop_the_server() {
        let mut n = net();
        let gone = client();
        connect(&mut n, &gone);
        drop(gone);
        n.begin_tick().unwrap(); // snapshot into a closed port

        let c = client();
        assert_eq!(connect(&mut n, &c).0, (2, 1));
    }

    #[test]
    fn an_oversized_datagram_is_dropped_and_the_server_goes_on() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let c = client();
        c.send_to(&vec![0u8; MAX_DATAGRAM * 2], to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().peers().count(), 0);
        #[cfg(windows)] // elsewhere it arrives truncated and fails decode instead
        assert_eq!(n.server().net_stats().get(OVERSIZE), 1);
        assert_eq!(connect(&mut n, &c).0, (1, 0));
    }

    /// Read one wrapped datagram off the relay's socket.
    fn read_wrapped(r: &UdpSocket) -> (SocketAddr, ServerMsg) {
        let mut buf = [0u8; MAX_DATAGRAM + ENVELOPE_MAX];
        let n = r.recv(&mut buf).expect("nothing came to the relay");
        let (to, sid, body) = unwrap(&KEY, Dir::Down, &mut buf[..n]).expect("origin sent the relay a bad envelope");
        assert_eq!(sid, SID, "a reply sealed for the wrong session");
        (to, decode(body).unwrap())
    }

    static KEY: std::sync::LazyLock<LinkKey> = std::sync::LazyLock::new(|| LinkKey::new([3; 32]));

    /// A datagram as the relay would forward it from client `c`.
    fn up(c: SocketAddr, d: &[u8]) -> Vec<u8> {
        up_as(c, &SID, d)
    }

    /// The same, under session `sid`.
    fn up_as(c: SocketAddr, sid: &Sid, d: &[u8]) -> Vec<u8> {
        wrap(&KEY, Dir::Up, c, sid, d)
    }

    const SID: Sid = Sid::from_bytes([1; 16]);

    /// Behind a relay, the origin talks to the relay alone. A client that
    /// found the origin's address gets nothing back, and nothing it sends is
    /// read as a player; the same Join through the relay is answered, to the
    /// relay, addressed to that client.
    #[test]
    fn behind_a_relay_only_the_relay_is_heard() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let relay = client();
        n.behind_relay(relay.local_addr().unwrap(), *KEY);
        let c = client();
        c.set_read_timeout(Some(Duration::from_millis(100))).unwrap();

        c.send_to(&join(), to).unwrap(); // straight at the origin
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().net_stats().get(NOT_RELAY), 1);

        let me = c.local_addr().unwrap();
        relay.send_to(&up(me, &join()), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        let (dest, msg) = read_wrapped(&relay);
        assert_eq!(dest, me);
        let ServerMsg::Challenge { cookie } = msg else { panic!("expected Challenge, got {msg:?}") };
        relay.send_to(&up(me, &join_with(Some(cookie))), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert!(matches!(read_wrapped(&relay), (d, ServerMsg::Joined { player_id: 1, .. }) if d == me));
        assert_eq!(n.server().player_id(me), Some(1), "admitted under the client's address, not the relay's");

        n.begin_tick().unwrap();
        assert!(matches!(read_wrapped(&relay), (d, ServerMsg::Snapshot { tick: 1, .. }) if d == me));
        let mut buf = [0u8; MAX_DATAGRAM];
        assert!(c.recv(&mut buf).is_err(), "the origin sent a client a datagram");
    }

    /// The session binding at its edges. A player admitted under one sid:
    /// someone with keys of their own (another sid) forging its address can
    /// neither re-Join in its name — which would move its replies to the
    /// forger's keys — nor send as it; nothing is answered. The player's own
    /// sid still plays. Once the player has left, its address is free and a
    /// new session binds.
    #[test]
    fn an_admitted_address_speaks_only_under_its_own_sid() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let relay = client();
        relay.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        n.behind_relay(relay.local_addr().unwrap(), *KEY);
        let me: SocketAddr = "10.1.2.3:4000".parse().unwrap();
        let forger = Sid::from_bytes([2; 16]);

        relay.send_to(&up(me, &join_with(Some(1))), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        let (_, msg) = read_wrapped(&relay);
        let ServerMsg::Joined { token, .. } = msg else { panic!("expected Joined, got {msg:?}") };

        relay.send_to(&up_as(me, &forger, &join_with(Some(1))), to).unwrap();
        relay.send_to(&up_as(me, &forger, &input(token, 1)), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap() && n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().net_stats().get(SID_MISMATCH), 2);
        let mut buf = [0u8; MAX_DATAGRAM + ENVELOPE_MAX];
        assert!(relay.recv(&mut buf).is_err(), "a forged re-Join was answered");

        relay.send_to(&up(me, &input(token, 1)), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        n.begin_tick().unwrap();
        assert!(
            matches!(read_wrapped(&relay), (d, ServerMsg::Snapshot { .. }) if d == me),
            "the player lost its replies"
        );

        // Idle past the timeout: the player leaves and the address is free.
        for _ in 0..=crate::server::IDLE_TICKS + 1 {
            n.begin_tick().unwrap();
        }
        assert_eq!(n.server().player_id(me), None);
        while relay.recv(&mut buf).is_ok() {} // the snapshots it was sent until then
        relay.send_to(&up_as(me, &forger, &join_with(Some(1))), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        let n2 = relay.recv(&mut buf).expect("the new session was not answered");
        let (dest, sid, _) = unwrap(&KEY, Dir::Down, &mut buf[..n2]).unwrap();
        assert_eq!((dest, sid), (me, forger));
        assert_eq!(n.server().net_stats().get(SID_MISMATCH), 2);
    }

    /// Edge: from the relay's own address, a datagram that is not an
    /// envelope is counted and dropped, not read as a client's.
    #[test]
    fn a_bad_envelope_from_the_relay_is_dropped() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let relay = client();
        n.behind_relay(relay.local_addr().unwrap(), *KEY);
        relay.send_to(&[9, 1, 2], to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().net_stats().get(BAD_ENVELOPE), 1);
        assert_eq!(n.server().net_stats().get(NOT_RELAY), 0);
    }

    /// The link MAC at its edge. A sender at the relay's own address — what
    /// forging that source address looks like to the origin — with the wrong
    /// key gets nothing: its Join is not answered and no one is admitted. A
    /// captured down envelope sent back up is refused too. The same Join
    /// under the right key is answered.
    #[test]
    fn the_relays_address_without_the_key_speaks_for_no_one() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let relay = client();
        relay.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        n.behind_relay(relay.local_addr().unwrap(), *KEY);
        let victim: SocketAddr = "10.9.9.9:4000".parse().unwrap();

        relay.send_to(&wrap(&LinkKey::new([4; 32]), Dir::Up, victim, &SID, &join()), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().net_stats().get(BAD_MAC), 1);
        let mut buf = [0u8; MAX_DATAGRAM + ENVELOPE_MAX];
        assert!(relay.recv(&mut buf).is_err(), "a forged envelope was answered");

        relay.send_to(&wrap(&KEY, Dir::Down, victim, &SID, &join()), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(n.server().net_stats().get(BAD_MAC), 2);
        assert!(relay.recv(&mut buf).is_err(), "a reversed envelope was answered");

        relay.send_to(&up(victim, &join()), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert!(matches!(read_wrapped(&relay), (d, ServerMsg::Challenge { .. }) if d == victim));
    }

    /// End to end: the token that makes an input count is the one that came
    /// back over the wire. The same input without it moves nobody.
    #[test]
    fn run_tick_applies_an_input_and_keeps_time() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let c = client();
        let (_, token) = connect(&mut n, &c);
        c.send_to(&input(NO_TOKEN, 1), to).unwrap(); // forged-looking: no token
        c.send_to(&input(token, 1), to).unwrap(); // both queued when tick 1 opens

        let t = Instant::now();
        let out = n.run_tick().unwrap();
        let took = t.elapsed();
        assert_eq!(out.steps, vec![(1, crate::sim::MOVE_SPEED)]);
        assert_eq!(n.server().net_stats().get("bad_token"), 1);
        assert!(took >= TICK, "tick ended early: {took:?}");
        // Loose upper bound: a busy CI box can overshoot, but not by 10 ticks.
        assert!(took < TICK * 10, "tick overran: {took:?}");
    }

    /// The backlog at its edge: with nobody draining, exactly `cap` datagrams
    /// wait and every one past that is dropped and counted — memory stays
    /// fixed however hard the socket is flooded.
    #[test]
    fn backlog_holds_exactly_its_cap_and_counts_the_rest() {
        const CAP: usize = 8;
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = sock.local_addr().unwrap();
        let r = Reader::spawn(sock, CAP).unwrap();
        let c = client();
        for i in 0..CAP as u8 + 5 {
            c.send_to(&[i], to).unwrap();
        }
        let t = Instant::now();
        while r.full.load(Ordering::Relaxed) < 5 && t.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(r.full.load(Ordering::Relaxed), 5);
        // the first CAP, in order
        for i in 0..CAP as u8 {
            match r.rx.recv_timeout(WAIT) {
                Ok(Ok(Rx::Datagram(_, d))) => assert_eq!(d, vec![i]),
                _ => panic!("datagram {i} missing"),
            }
        }
        assert!(r.rx.try_recv().is_err(), "more than CAP were kept");
    }

    /// Real-time ticks lose nothing. A sender streams datagrams across many
    /// tick boundaries — every tick's read loop ends on its deadline, the
    /// moment a datagram can be in flight — and every one is read: behind a
    /// relay at an address no one uses, each datagram is exactly one
    /// `not_relay`, so the count is exact.
    #[test]
    fn real_time_ticks_read_every_datagram() {
        const N: u64 = 20000;
        let mut n = net();
        let to = n.local_addr().unwrap();
        n.behind_relay("127.0.0.1:9".parse().unwrap(), LinkKey::new([0; 32]));
        let sender = std::thread::spawn(move || {
            let s = UdpSocket::bind("127.0.0.1:0").unwrap();
            for i in 0..N {
                s.send_to(&[1, 2, 3], to).unwrap();
                if i % 16 == 0 {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        });
        let start = Instant::now();
        while !sender.is_finished() || n.server().net_stats().get(NOT_RELAY) < N {
            n.run_tick().unwrap();
            if start.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        sender.join().unwrap();
        assert_eq!(n.server().net_stats().get(NOT_RELAY), N, "datagrams lost between ticks");
    }
}
