//! Aegis relay — pillar D's "no one sees the real server IP".
//!
//! Clients are given the relay's address and nothing else. The relay forwards
//! every client datagram to the origin [`wrap`]ped with the client's address,
//! and every wrapped reply from the origin back to the client it names, from
//! the relay's own socket. The origin (`NetServer::behind_relay`) hears only
//! the relay, so its address never has to be published: a room-DDoS has
//! nothing to aim at but the relay, and relays are the cheap, replaceable,
//! scalable part.
//!
//! Two sockets, two threads:
//!
//! ```text
//! client --> public   --[up]-->   upstream --> origin   (wrap: client addr + datagram)
//! client <-- public  <--[down]--  upstream <-- origin   (unwrap: send payload to that addr)
//! ```
//!
//! Edge filter (D5.2), stateless — no table, nothing to sync, a restart
//! loses nothing. A client datagram is dropped here, and never crosses to
//! the origin, when it
//!   - is too short to carry a session token (it cannot be a client
//!     message at all), or
//!   - carries a token that was not issued for its source address: the
//!     origin MACs every token under the link key ([`token_valid`]), so the
//!     relay checks it without remembering it.
//!
//! A token of 0 (a Join) passes the token check. But token 0 is free to send,
//! so it is the one thing that check cannot thin — those datagrams get the
//! origin's own unauthenticated budget here instead ([`join_rate`], the only
//! state the relay keeps, cleared every window). Within it, a Join must
//! prove its address before it crosses ([`cookie`], D5.4): a cookieless Join
//! is answered here with a challenge and goes no further, and a Join whose
//! cookie the relay did not issue to that address is dropped. So no Join
//! from a forged source — from one IP or from thousands — reaches the
//! origin. Version and sessions per IP are still judged at the origin, and
//! every other guard still runs there; the relay only takes away traffic the
//! origin would certainly have refused, and answers challenges the origin
//! would have answered — so what reaches the origin's guards, and the
//! telemetry they write, is unchanged (the harness asserts it byte for byte).
//!
//! The window is the origin's tick. A deployed relay keeps it by the wall
//! clock ([`Clock::Wall`]), so its windows and the origin's ticks are not
//! aligned: a burst across a boundary can be counted in one window here and
//! two ticks there, and the relay is stricter than the origin by at most
//! that. Only a sender past the budget is ever affected. The harness runs in
//! lockstep ([`Clock::Lockstep`]) and moves the window with each tick.
//!
//! The link is sealed: relay and origin share a [`LinkKey`] and every
//! envelope is encrypted and authenticated under it (XChaCha20-Poly1305),
//! each direction distinct. Forging the relay's source address toward the
//! origin gets nobody admitted; forging the origin's toward the relay gets
//! nothing reflected; watching the link shows no client address, token or
//! message.
//!
//! The client's leg is sealed too (protocol v3). The game's backend, which
//! shares an [`EdgeKey`] with the relay, hands each client a session id and
//! two keys over HTTPS ([`aegis_protocol::mint_connect`]); every datagram
//! the client sends is sealed under one of them, every datagram it gets
//! under the other. The relay re-derives both from the sid at the front of
//! each datagram — no table, no public-key operation — so the relay stays
//! stateless. A datagram that does not open is dropped before any other
//! check (`bad_seal`). The sid travels to the origin in the envelope and
//! comes back with each reply, which is how the relay knows whose keys a
//! reply is sealed under. A sid stops admitting Joins at its expiry; a
//! session already admitted plays on.
//!
//! LIMITS: an on-path observer of either leg still sees sizes, timing and
//! (on the client's leg) the sid, and can replay a captured datagram the way
//! it went (the session token and replay guard judge that). Anyone holding
//! keys the backend issued — any player — can still send from a forged
//! source address; the token, the cookie and the origin's sid binding are
//! what stop that from acting as someone else. A token-0 flood spread over
//! many forged source IPs still costs the relay one key derivation and one
//! open per datagram and a challenge per IP per window (never larger than
//! the Join that asked), but none of it crosses. And a relay only hides an
//! origin that is otherwise unreachable: one with a public address and no
//! firewall can still be found and flooded directly.

pub mod cookie;
pub mod join_rate;

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use aegis_protocol::{
    decode, encode, open_up, seal_down, split_frame, token_valid, unwrap, wrap, ClientMsg, Dir, EdgeError, EdgeKey,
    EnvelopeError, LinkKey, ServerMsg, EDGE_UP_MAX, ENVELOPE_MAX, MAX_DATAGRAM, NO_TOKEN, TICK_HZ,
};
use join_rate::JoinRate;

/// What the relay's budget window follows.
#[derive(Clone)]
pub enum Clock {
    /// One window per origin tick of wall-clock time, from spawn.
    Wall(Instant),
    /// Moved only by [`Relay::next_window`] — for a lockstep harness.
    Lockstep(Arc<AtomicU32>),
}

impl Clock {
    pub fn wall() -> Self {
        Clock::Wall(Instant::now())
    }

    pub fn lockstep() -> Self {
        Clock::Lockstep(Arc::new(AtomicU32::new(0)))
    }

    fn window(&self) -> u32 {
        match self {
            Clock::Wall(t0) => (t0.elapsed().as_nanos() * u128::from(TICK_HZ) / 1_000_000_000) as u32,
            Clock::Lockstep(w) => w.load(Ordering::SeqCst),
        }
    }
}

/// WSAEMSGSIZE: the datagram was larger than the buffer, and is gone.
const WSAEMSGSIZE: i32 = 10040;

/// What the relay has done, counted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayStats {
    /// Client datagrams forwarded to the origin.
    pub up: u64,
    /// Origin datagrams forwarded to a client.
    pub down: u64,
    /// Client datagrams whose frame is larger than `MAX_DATAGRAM`, dropped.
    pub oversize: u64,
    /// Client datagrams too short to carry a seal, or whose frame is too
    /// short to carry a token, dropped.
    pub short: u64,
    /// Client datagrams that did not open under the keys their sid names:
    /// not from a session the backend issued, or altered. Dropped.
    pub bad_seal: u64,
    /// Joins (token 0) under a sid past its expiry, dropped. A session
    /// already admitted keeps playing on it.
    pub expired: u64,
    /// Client datagrams with a token not issued for their source, dropped.
    pub bad_token: u64,
    /// Token-0 client datagrams past their IP's budget for the window, dropped.
    pub join_rate: u64,
    /// Cookieless Joins answered here with a challenge (not forwarded, and
    /// not dropped either: the client was answered).
    pub challenged: u64,
    /// Joins with a cookie the relay did not issue to their source, dropped.
    pub bad_cookie: u64,
    /// Datagrams at the upstream socket from anyone but the origin, dropped.
    pub foreign: u64,
    /// Datagrams from the origin's address whose MAC was wrong, dropped.
    pub bad_mac: u64,
    /// Authentic datagrams from the origin that were not an envelope, dropped.
    pub bad_envelope: u64,
}

#[derive(Default)]
struct Counters {
    up: AtomicU64,
    down: AtomicU64,
    oversize: AtomicU64,
    short: AtomicU64,
    bad_seal: AtomicU64,
    expired: AtomicU64,
    bad_token: AtomicU64,
    join_rate: AtomicU64,
    challenged: AtomicU64,
    bad_cookie: AtomicU64,
    foreign: AtomicU64,
    bad_mac: AtomicU64,
    bad_envelope: AtomicU64,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

pub struct Relay {
    public: SocketAddr,
    upstream: SocketAddr,
    counters: Arc<Counters>,
    clock: Clock,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<io::Result<()>>>,
}

impl Relay {
    /// Bind the public socket (what clients are told) and the upstream socket
    /// (what the origin allowlists), and start forwarding to `origin`, with
    /// whom this relay shares `key`; clients' sessions are sealed under keys
    /// derived from `edge`, which this relay shares with the game's backend.
    /// Budget windows follow the wall clock.
    pub fn spawn(
        public: impl ToSocketAddrs,
        upstream: impl ToSocketAddrs,
        origin: SocketAddr,
        key: LinkKey,
        edge: EdgeKey,
    ) -> io::Result<Self> {
        Self::spawn_with(public, upstream, origin, key, edge, Clock::wall())
    }

    /// The same, with budget windows following `clock`.
    pub fn spawn_with(
        public: impl ToSocketAddrs,
        upstream: impl ToSocketAddrs,
        origin: SocketAddr,
        key: LinkKey,
        edge: EdgeKey,
        clock: Clock,
    ) -> io::Result<Self> {
        let keys = Keys { link: key, edge };
        // Blocking reads, no timeout: on Windows a UDP read that times out
        // while a datagram is arriving can lose it (measured: 1 join in 13
        // vanished with a 10 ms poll under parallel load). Drop wakes the
        // threads with a datagram instead.
        let public = UdpSocket::bind(public)?;
        let upstream = UdpSocket::bind(upstream)?;
        let (public_addr, upstream_addr) = (public.local_addr()?, upstream.local_addr()?);
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));

        let up = {
            let (public, upstream) = (public.try_clone()?, upstream.try_clone()?);
            let (c, stop, clock) = (counters.clone(), stop.clone(), clock.clone());
            std::thread::spawn(move || forward_up(&public, &upstream, origin, &keys, &clock, &c, &stop))
        };
        let down = {
            let (c, stop) = (counters.clone(), stop.clone());
            std::thread::spawn(move || forward_down(&upstream, &public, origin, &keys, &c, &stop))
        };
        Ok(Self { public: public_addr, upstream: upstream_addr, counters, clock, stop, threads: vec![up, down] })
    }

    /// Start the next budget window. Lockstep only: call it when the origin
    /// starts a tick, before any of that tick's datagrams are sent.
    ///
    /// # Panics
    /// On a wall-clock relay, which moves its own windows.
    pub fn next_window(&self) {
        match &self.clock {
            Clock::Lockstep(w) => {
                w.fetch_add(1, Ordering::SeqCst);
            }
            Clock::Wall(_) => panic!("a wall-clock relay moves its own windows"),
        }
    }

    /// The address clients send to — the only server address they know.
    pub fn public_addr(&self) -> SocketAddr {
        self.public
    }

    /// The address the relay talks to the origin from — what the origin
    /// allowlists.
    pub fn upstream_addr(&self) -> SocketAddr {
        self.upstream
    }

    pub fn stats(&self) -> RelayStats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        RelayStats {
            up: get(&c.up),
            down: get(&c.down),
            oversize: get(&c.oversize),
            short: get(&c.short),
            bad_seal: get(&c.bad_seal),
            expired: get(&c.expired),
            bad_token: get(&c.bad_token),
            join_rate: get(&c.join_rate),
            challenged: get(&c.challenged),
            bad_cookie: get(&c.bad_cookie),
            foreign: get(&c.foreign),
            bad_mac: get(&c.bad_mac),
            bad_envelope: get(&c.bad_envelope),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // One empty datagram to each socket unblocks its thread, which sees
        // the flag before handling it. A socket bound to the unspecified
        // address is reached on loopback.
        let reach = |a: SocketAddr| match a.ip() {
            ip if ip.is_unspecified() && a.is_ipv4() => SocketAddr::new(Ipv4Addr::LOCALHOST.into(), a.port()),
            ip if ip.is_unspecified() => SocketAddr::new(Ipv6Addr::LOCALHOST.into(), a.port()),
            _ => a,
        };
        for a in [self.public, self.upstream] {
            let to = reach(a);
            let local: SocketAddr =
                if to.is_ipv4() { (Ipv4Addr::LOCALHOST, 0).into() } else { (Ipv6Addr::LOCALHOST, 0).into() };
            if let Ok(w) = UdpSocket::bind(local) {
                let _ = w.send_to(&[], to);
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Windows reporting that an earlier send went to a closed port. Says
/// nothing about this socket; one client leaving must not stop the relay.
fn is_reset(e: &io::Error) -> bool {
    e.kind() == ErrorKind::ConnectionReset
}

/// The two secrets a relay holds: one shared with its origin, one with the
/// game's backend.
#[derive(Clone, Copy)]
struct Keys {
    link: LinkKey,
    edge: EdgeKey,
}

/// Unix seconds now, for sid expiry. A clock before 1970 reads as 0 and so
/// expires nothing — the failure that keeps players playing.
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn forward_up(
    public: &UdpSocket,
    upstream: &UdpSocket,
    origin: SocketAddr,
    keys: &Keys,
    clock: &Clock,
    c: &Counters,
    stop: &AtomicBool,
) -> io::Result<()> {
    let key = &keys.link;
    // One byte more than the largest legal sealed datagram, so a larger one
    // is seen as larger (and not as cut to fit) on every OS.
    let mut buf = vec![0u8; MAX_DATAGRAM + EDGE_UP_MAX + 1];
    let mut joins = JoinRate::new();
    loop {
        let got = public.recv_from(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (n, client) = match got {
            Ok(v) => v,
            Err(e) if e.raw_os_error() == Some(WSAEMSGSIZE) => {
                bump(&c.oversize);
                continue;
            }
            Err(e) if is_reset(&e) => continue,
            Err(e) => return Err(e),
        };
        if n > MAX_DATAGRAM + EDGE_UP_MAX {
            bump(&c.oversize);
            continue;
        }
        let (sid, frame) = match open_up(&keys.edge, &mut buf[..n]) {
            Ok(v) => v,
            Err(EdgeError::Short) => {
                bump(&c.short);
                continue;
            }
            Err(EdgeError::BadSeal) => {
                bump(&c.bad_seal);
                continue;
            }
        };
        match split_frame(frame) {
            None => bump(&c.short),
            Some((token, _)) if token != NO_TOKEN && !token_valid(key, client, token) => bump(&c.bad_token),
            Some((NO_TOKEN, _)) if sid.expires() < unix_now() => bump(&c.expired),
            Some((NO_TOKEN, body)) => {
                let window = clock.window();
                if !joins.allow(window, client.ip()) {
                    bump(&c.join_rate);
                    continue;
                }
                match decode::<ClientMsg>(body) {
                    Ok(ClientMsg::Join { cookie: None, .. }) => {
                        let challenge = ServerMsg::Challenge { cookie: cookie::issue(key, client, window) };
                        let _ = public.send_to(&seal_down(&keys.edge, &sid, &encode(&challenge)), client);
                        bump(&c.challenged);
                    }
                    Ok(ClientMsg::Join { cookie: Some(k), .. }) if !cookie::valid(key, client, window, k) => {
                        bump(&c.bad_cookie)
                    }
                    // A proven Join, or anything else with token 0: the
                    // origin judges it (and refuses all but the Join).
                    _ => {
                        let _ = upstream.send_to(&wrap(key, Dir::Up, client, &sid, frame), origin);
                        bump(&c.up);
                    }
                }
            }
            Some(_) => {
                let _ = upstream.send_to(&wrap(key, Dir::Up, client, &sid, frame), origin);
                bump(&c.up);
            }
        }
    }
}

fn forward_down(
    upstream: &UdpSocket,
    public: &UdpSocket,
    origin: SocketAddr,
    keys: &Keys,
    c: &Counters,
    stop: &AtomicBool,
) -> io::Result<()> {
    let key = &keys.link;
    let mut buf = vec![0u8; MAX_DATAGRAM + ENVELOPE_MAX];
    loop {
        let got = upstream.recv_from(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match got {
            Ok((_, from)) if from != origin => bump(&c.foreign),
            Ok((n, _)) => match unwrap(key, Dir::Down, &mut buf[..n]) {
                Ok((client, sid, payload)) => {
                    let _ = public.send_to(&seal_down(&keys.edge, &sid, payload), client);
                    bump(&c.down);
                }
                Err(EnvelopeError::BadMac) => bump(&c.bad_mac),
                Err(_) => bump(&c.bad_envelope),
            },
            Err(e) if is_reset(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::{mint_connect, mint_token, open_down, seal_up, ClientKeys, Sid};
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(1);
    static KEY: std::sync::LazyLock<LinkKey> = std::sync::LazyLock::new(|| LinkKey::new([5; 32]));
    static EDGE: std::sync::LazyLock<EdgeKey> = std::sync::LazyLock::new(|| EdgeKey::new([6; 32]));

    /// A session good for an hour, as the backend would issue one.
    fn session() -> ClientKeys {
        mint_connect(&EDGE, unix_now() + 3600)
    }

    fn spawn(origin: &UdpSocket) -> Relay {
        Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), *KEY, *EDGE).unwrap()
    }

    fn spawn_lockstep(origin: &UdpSocket) -> Relay {
        Relay::spawn_with("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), *KEY, *EDGE, Clock::lockstep())
            .unwrap()
    }

    fn sock() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(WAIT)).unwrap();
        s
    }

    fn recv(s: &UdpSocket) -> (Vec<u8>, SocketAddr) {
        let mut buf = vec![0u8; MAX_DATAGRAM + ENVELOPE_MAX + EDGE_UP_MAX + 1];
        let (n, from) = s.recv_from(&mut buf).expect("nothing arrived");
        buf.truncate(n);
        (buf, from)
    }

    /// What a client reads: one datagram, opened under its keys.
    fn recv_client(s: &UdpSocket, k: &ClientKeys) -> (Vec<u8>, SocketAddr) {
        let (mut wire, from) = recv(s);
        let plain = open_down(k, &mut wire).expect("the client could not open what the relay sent").to_vec();
        (plain, from)
    }

    /// A client datagram: `token`, then `body`.
    fn tokened(token: u64, body: &[u8]) -> Vec<u8> {
        let mut d = token.to_le_bytes().to_vec();
        d.extend(body);
        d
    }

    /// The envelope's contents at the origin, owned.
    fn opened(wire: &mut [u8]) -> (SocketAddr, Sid, Vec<u8>) {
        let (c, sid, p) = unwrap(&KEY, Dir::Up, wire).expect("not an envelope under the key");
        (c, sid, p.to_vec())
    }

    /// Wait (bounded) until the relay's counters say `done`.
    fn settle(r: &Relay, done: impl Fn(RelayStats) -> bool) -> RelayStats {
        let t = Instant::now();
        while !done(r.stats()) && t.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(1));
        }
        r.stats()
    }

    /// Both directions: the origin gets the client's datagram opened and
    /// wrapped with the client's address and sid, from the upstream address;
    /// the client gets the origin's reply sealed under its session, from the
    /// public address — never from the origin, never in the clear.
    #[test]
    fn forwards_both_ways_and_the_client_only_sees_the_relay() {
        let origin = sock();
        let r = spawn(&origin);
        let client = sock();
        let me = client.local_addr().unwrap();
        let k = session();

        let hello = tokened(NO_TOKEN, b"hello");
        client.send_to(&seal_up(&k, &hello), r.public_addr()).unwrap();
        let (mut got, from) = recv(&origin);
        assert_eq!(from, r.upstream_addr());
        assert_eq!(opened(&mut got), (me, k.sid, hello));

        origin.send_to(&wrap(&KEY, Dir::Down, me, &k.sid, b"back"), r.upstream_addr()).unwrap();
        let (wire, from) = recv(&client);
        assert_eq!(from, r.public_addr());
        assert_ne!(from, origin.local_addr().unwrap());
        assert!(!wire.windows(4).any(|w| w == b"back"), "the reply crossed the client's leg in the clear");
        assert_eq!(open_down(&k, &mut wire.clone()), Ok(&b"back"[..]));
        // Each thread counts after it sends, so the reader can be first.
        assert_eq!(settle(&r, |s| s.up == 1 && s.down == 1), RelayStats { up: 1, down: 1, ..Default::default() });
    }

    /// What crosses the link, read raw off the wire as an on-path observer
    /// would: the client's address, its session token and its message are
    /// nowhere in it — and it still opens, under the key, to exactly what
    /// the client sent.
    #[test]
    fn the_link_carries_nothing_in_the_clear() {
        let origin = sock();
        let r = spawn(&origin);
        let client = sock();
        let me = client.local_addr().unwrap();
        let k = session();
        let token = mint_token(&KEY, me, 0xC0FF_EE11);
        let sent = tokened(token, b"aim-at-player-7");
        client.send_to(&seal_up(&k, &sent), r.public_addr()).unwrap();
        let (mut wire, _) = recv(&origin);
        let has = |needle: &[u8]| wire.windows(needle.len()).any(|w| w == needle);
        assert!(!has(&token.to_le_bytes()), "token in the clear");
        assert!(!has(b"aim-at-player-7"), "message in the clear");
        assert!(!has(&[127, 0, 0, 1]), "client IP in the clear");
        assert!(!has(k.sid.as_bytes()), "sid in the clear");
        assert_eq!(opened(&mut wire), (me, k.sid, sent));
    }

    /// The hole this closes, at the relay: the token the origin issues, and
    /// the input that carries it, never cross the client's leg readable. An
    /// observer there sees the sid and noise.
    #[test]
    fn the_client_leg_carries_no_token() {
        let origin = sock();
        let r = spawn(&origin);
        let client = sock();
        let me = client.local_addr().unwrap();
        let k = session();
        let token = mint_token(&KEY, me, 0x7777_0001);
        let joined = encode(&ServerMsg::Joined { player_id: 3, token, tick: 1 });
        origin.send_to(&wrap(&KEY, Dir::Down, me, &k.sid, &joined), r.upstream_addr()).unwrap();
        let (down, _) = recv(&client);
        assert!(!down.windows(8).any(|w| w == token.to_le_bytes()), "issued token readable on the client's leg");
        let up = seal_up(&k, &tokened(token, b"input"));
        assert!(!up.windows(8).any(|w| w == token.to_le_bytes()), "token readable in the client's input");
        assert_eq!(open_down(&k, &mut down.clone()), Ok(&joined[..]));
        drop(r);
    }

    /// Only a datagram sealed under keys the backend issued crosses. A
    /// pre-v3 client's plain frame, a seal under another backend's edge key,
    /// one altered byte of the sid, and anything shorter than the seal are
    /// all dropped before any other check — and nothing is sent back.
    #[test]
    fn only_a_session_the_backend_issued_crosses() {
        let origin = sock();
        origin.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let r = spawn_lockstep(&origin);
        let client = sock();
        client.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let k = session();
        let join = tokened(NO_TOKEN, b"join-of-a-reasonable-length-for-a-seal-to-fit-around-it");
        let send = |d: &[u8]| client.send_to(d, r.public_addr()).unwrap();

        send(&join); // plain, as before v3
        send(&seal_up(&mint_connect(&EdgeKey::new([1; 32]), unix_now() + 60), &join));
        let mut altered = seal_up(&k, &join);
        altered[SID_EXPIRY_BYTE] ^= 1; // a later expiry nobody issued
        send(&altered);
        send(&seal_up(&k, &join)[..EDGE_UP_MAX - 1]);
        let s = settle(&r, |s| s.bad_seal + s.short == 4);
        assert_eq!(s, RelayStats { bad_seal: 3, short: 1, ..Default::default() });
        let mut buf = [0u8; 256];
        assert!(origin.recv(&mut buf).is_err(), "an unsealed datagram crossed");
        assert!(client.recv(&mut buf).is_err(), "the relay answered an unsealed datagram");
    }

    /// Byte 0 of a sid is the low byte of its expiry.
    const SID_EXPIRY_BYTE: usize = 0;

    /// Only the origin can make the relay send to a client: anyone else
    /// writing to the upstream socket is dropped — otherwise the relay would
    /// reflect whatever a stranger wrapped at whatever address it named.
    #[test]
    fn upstream_ignores_everyone_but_the_origin() {
        let origin = sock();
        let r = spawn(&origin);
        let victim = sock();
        victim.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let stranger = sock();
        let k = session();
        stranger
            .send_to(&wrap(&KEY, Dir::Down, victim.local_addr().unwrap(), &k.sid, b"x"), r.upstream_addr())
            .unwrap();
        let s = settle(&r, |s| s.foreign == 1);
        assert_eq!(s, RelayStats { foreign: 1, ..Default::default() });
        let mut buf = [0u8; 64];
        assert!(victim.recv(&mut buf).is_err(), "the relay reflected a stranger's datagram");
    }

    #[test]
    fn a_bad_envelope_from_the_origin_is_dropped() {
        let origin = sock();
        let r = spawn(&origin);
        origin.send_to(&[9, 9], r.upstream_addr()).unwrap();
        assert_eq!(settle(&r, |s| s.bad_envelope == 1), RelayStats { bad_envelope: 1, ..Default::default() });
    }

    /// The down-link MAC at its edge: from the origin's own address — what
    /// forging that source address looks like — an envelope under the wrong
    /// key, or one captured going up, is not forwarded. The relay cannot be
    /// made to reflect at a victim without the key.
    #[test]
    fn the_origins_address_without_the_key_reflects_nothing() {
        let origin = sock();
        let r = spawn(&origin);
        let victim = sock();
        victim.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let v = victim.local_addr().unwrap();
        let k = session();
        origin.send_to(&wrap(&LinkKey::new([6; 32]), Dir::Down, v, &k.sid, b"flood"), r.upstream_addr()).unwrap();
        origin.send_to(&wrap(&KEY, Dir::Up, v, &k.sid, b"flood"), r.upstream_addr()).unwrap();
        assert_eq!(settle(&r, |s| s.bad_mac == 2), RelayStats { bad_mac: 2, ..Default::default() });
        let mut buf = [0u8; 64];
        assert!(victim.recv(&mut buf).is_err(), "the relay forwarded a forged envelope");
        // under the key, the same thing goes through
        origin.send_to(&wrap(&KEY, Dir::Down, v, &k.sid, b"ok"), r.upstream_addr()).unwrap();
        assert_eq!(recv_client(&victim, &k).0, b"ok");
    }

    /// Edge: a client frame of exactly MAX_DATAGRAM, sealed, is forwarded
    /// whole; one byte more is dropped as oversize — on every OS, since the
    /// relay reads one byte past the largest legal datagram.
    #[test]
    fn max_datagram_goes_through_one_byte_more_does_not() {
        let origin = sock();
        let r = spawn(&origin);
        let client = sock();
        let k = session();
        client.send_to(&seal_up(&k, &tokened(NO_TOKEN, &[7u8; MAX_DATAGRAM - 8])), r.public_addr()).unwrap();
        let (mut got, _) = recv(&origin);
        assert_eq!(opened(&mut got).2.len(), MAX_DATAGRAM);

        client.send_to(&seal_up(&k, &tokened(NO_TOKEN, &[7u8; MAX_DATAGRAM - 7])), r.public_addr()).unwrap();
        let s = settle(&r, |s| s.oversize + s.up >= 2);
        assert_eq!((s.up, s.oversize), (1, 1));
    }

    /// The edge filter at its edges, inside the seal. From one client: a
    /// frame one byte too short to hold a token, a token minted for another
    /// port, a guess, and a token minted under another key are all dropped
    /// at the relay; a token minted for exactly this address and a token of
    /// 0 (a Join) cross.
    #[test]
    fn only_a_token_issued_for_this_address_crosses() {
        let origin = sock();
        let r = spawn(&origin);
        let client = sock();
        let me = client.local_addr().unwrap();
        let k = session();
        let other_port = SocketAddr::new(me.ip(), me.port().wrapping_add(1));
        let send = |d: &[u8]| client.send_to(&seal_up(&k, d), r.public_addr()).unwrap();

        send(&[0u8; 7]); // 7 bytes: no token
        send(&tokened(mint_token(&KEY, other_port, 1), b"x"));
        send(&tokened(0x1234_5678_9ABC_DEF0, b"x"));
        send(&tokened(mint_token(&LinkKey::new([1; 32]), me, 1), b"x"));
        let s = settle(&r, |s| s.short + s.bad_token == 4);
        assert_eq!((s.short, s.bad_token, s.up), (1, 3, 0));

        let good = tokened(mint_token(&KEY, me, 1), b"in");
        send(&good);
        assert_eq!(opened(&mut recv(&origin).0), (me, k.sid, good));
        let join = tokened(NO_TOKEN, b"join");
        send(&join);
        assert_eq!(opened(&mut recv(&origin).0), (me, k.sid, join));
        // Exactly 8 bytes is a token and an empty body: it crosses (the
        // origin's decoder refuses the body).
        send(&tokened(NO_TOKEN, b""));
        assert_eq!(opened(&mut recv(&origin).0).2.len(), 8);
        assert_eq!(settle(&r, |s| s.up == 3).up, 3);
    }

    /// The token-0 budget at its edge, over real sockets. In one window: the
    /// cap crosses, cap + 1 is dropped, a datagram with a valid token still
    /// crosses (it has its own budget at the origin), and another port on
    /// the same IP gets nothing more. The next window crosses again.
    #[test]
    fn token_zero_gets_the_origins_budget_per_ip_per_window() {
        use join_rate::MAX_PER_WINDOW;
        let origin = sock();
        let r = spawn_lockstep(&origin);
        let (a, b) = (sock(), sock());
        let (ka, kb) = (session(), session());
        let join = tokened(NO_TOKEN, b"join");
        for _ in 0..MAX_PER_WINDOW {
            a.send_to(&seal_up(&ka, &join), r.public_addr()).unwrap();
        }
        b.send_to(&seal_up(&kb, &join), r.public_addr()).unwrap(); // same IP, other port
        let input = tokened(mint_token(&KEY, a.local_addr().unwrap(), 1), b"in");
        a.send_to(&seal_up(&ka, &input), r.public_addr()).unwrap();
        let s = settle(&r, |s| s.up + s.join_rate == u64::from(MAX_PER_WINDOW) + 2);
        assert_eq!((s.up, s.join_rate), (u64::from(MAX_PER_WINDOW) + 1, 1));
        for _ in 0..=MAX_PER_WINDOW {
            recv(&origin);
        }

        r.next_window();
        b.send_to(&seal_up(&kb, &join), r.public_addr()).unwrap();
        assert_eq!(opened(&mut recv(&origin).0).0, b.local_addr().unwrap());
        assert_eq!(r.stats().join_rate, 1);
    }

    fn join(cookie: Option<u64>) -> Vec<u8> {
        aegis_protocol::frame(
            NO_TOKEN,
            &ClientMsg::Join { name: "j".into(), protocol: aegis_protocol::PROTOCOL_VERSION, cookie },
        )
    }

    /// The handshake at the edge: a cookieless Join is answered by the
    /// relay, from its public address, sealed for the session that asked,
    /// and nothing crosses; the Join that brings the cookie back crosses
    /// whole.
    #[test]
    fn a_cookieless_join_is_challenged_at_the_edge_and_the_answer_crosses() {
        let origin = sock();
        origin.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let r = spawn_lockstep(&origin);
        let client = sock();
        let k = session();
        client.send_to(&seal_up(&k, &join(None)), r.public_addr()).unwrap();
        let (got, from) = recv_client(&client, &k);
        assert_eq!(from, r.public_addr());
        let ServerMsg::Challenge { cookie: c } = decode::<ServerMsg>(&got).unwrap() else { panic!("not a challenge") };
        assert_eq!(c, cookie::issue(&KEY, client.local_addr().unwrap(), 0));
        let mut buf = [0u8; 64];
        assert!(origin.recv(&mut buf).is_err(), "a cookieless join crossed");

        let answer = join(Some(c));
        client.send_to(&seal_up(&k, &answer), r.public_addr()).unwrap();
        assert_eq!(opened(&mut recv(&origin).0), (client.local_addr().unwrap(), k.sid, answer));
        assert_eq!(r.stats(), RelayStats { up: 1, challenged: 1, ..Default::default() });
    }

    /// A Join whose cookie was not issued to its source — a guess, another
    /// port's, another key's, or one two buckets old — is dropped at the
    /// edge. One from the last bucket still crosses.
    #[test]
    fn a_cookie_not_issued_to_this_address_is_dropped() {
        let origin = sock();
        let r = spawn_lockstep(&origin);
        let client = sock();
        let k = session();
        let me = client.local_addr().unwrap();
        let other = SocketAddr::new(me.ip(), me.port().wrapping_add(1));
        let old = cookie::issue(&KEY, me, 0);
        for _ in 0..2 * cookie::BUCKET_WINDOWS {
            r.next_window();
        }
        for c in [
            0x5EED,
            cookie::issue(&KEY, other, 2 * cookie::BUCKET_WINDOWS),
            cookie::issue(&LinkKey::new([1; 32]), me, 0),
            old,
        ] {
            client.send_to(&seal_up(&k, &join(Some(c))), r.public_addr()).unwrap();
        }
        assert_eq!(settle(&r, |s| s.bad_cookie == 4), RelayStats { bad_cookie: 4, ..Default::default() });
        let last = cookie::issue(&KEY, me, cookie::BUCKET_WINDOWS);
        client.send_to(&seal_up(&k, &join(Some(last))), r.public_addr()).unwrap();
        assert_eq!(opened(&mut recv(&origin).0).0, me);
    }

    /// Joins forged from many source IPs — each with a fresh token-0 budget,
    /// all under keys the backend issued — get a challenge each and none
    /// crosses. (Loopback is 127.0.0.0/8, so these are real distinct source
    /// IPs.)
    #[test]
    fn joins_from_many_ips_never_cross_unproven() {
        let origin = sock();
        origin.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let r = spawn_lockstep(&origin);
        let k = session();
        const IPS: u8 = 50;
        for i in 0..IPS {
            let s = UdpSocket::bind((Ipv4Addr::new(127, 0, 9, i + 1), 0)).unwrap();
            for _ in 0..3 {
                s.send_to(&seal_up(&k, &join(None)), r.public_addr()).unwrap();
            }
        }
        let s = settle(&r, |s| s.challenged == 3 * u64::from(IPS));
        assert_eq!(s, RelayStats { challenged: 3 * u64::from(IPS), ..Default::default() });
        let mut buf = [0u8; 64];
        assert!(origin.recv(&mut buf).is_err(), "an unproven join crossed");
    }

    /// Expiry at its edge. A sid past its expiry admits no Join — not
    /// challenged, not forwarded — but a player already admitted under it
    /// (an input with a token issued for this address) plays on. A sid
    /// expiring this very second still admits.
    #[test]
    fn an_expired_sid_admits_no_join_but_keeps_its_player() {
        let origin = sock();
        origin.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let r = spawn_lockstep(&origin);
        let client = sock();
        let me = client.local_addr().unwrap();
        let old = mint_connect(&EDGE, unix_now() - 1);
        client.send_to(&seal_up(&old, &join(None)), r.public_addr()).unwrap();
        assert_eq!(settle(&r, |s| s.expired == 1), RelayStats { expired: 1, ..Default::default() });

        let input = tokened(mint_token(&KEY, me, 9), b"in");
        client.send_to(&seal_up(&old, &input), r.public_addr()).unwrap();
        assert_eq!(opened(&mut recv(&origin).0), (me, old.sid, input));

        // Still good until the second after its expiry. (Minted far enough
        // ahead that the test cannot straddle it.)
        let edge = mint_connect(&EDGE, unix_now() + 2);
        client.send_to(&seal_up(&edge, &join(None)), r.public_addr()).unwrap();
        assert!(matches!(decode::<ServerMsg>(&recv_client(&client, &edge).0), Ok(ServerMsg::Challenge { .. })));
    }

    #[test]
    #[should_panic(expected = "moves its own windows")]
    fn a_wall_clock_relay_cannot_be_stepped() {
        let origin = sock();
        let r = spawn(&origin);
        r.next_window();
    }

    /// Drop stops both threads promptly.
    #[test]
    fn drop_stops_the_threads() {
        let origin = sock();
        let r = spawn(&origin);
        let t = Instant::now();
        drop(r);
        assert!(t.elapsed() < WAIT);
    }
}
