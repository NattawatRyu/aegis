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
//! A token of 0 (a Join) passes: joins are judged at the origin (cookie,
//! version, per-IP budget). Every guard still runs at the origin; the relay
//! only takes away traffic the origin would certainly have refused — so what
//! reaches the origin's guards, and the telemetry they write, is unchanged
//! (the harness asserts it byte for byte).
//!
//! The link is authenticated: relay and origin share a [`LinkKey`] and every
//! envelope carries a MAC under it, each direction distinct. Forging the
//! relay's source address toward the origin gets nobody admitted; forging the
//! origin's toward the relay gets nothing reflected.
//!
//! LIMITS: not encrypted — an on-path observer of the link reads client
//! addresses and tokens, and can replay a captured envelope the way it went.
//! And a relay only hides an origin that is otherwise unreachable: one with a
//! public address and no firewall can still be found and flooded directly.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use aegis_protocol::{
    split_frame, token_valid, unwrap, wrap, Dir, EnvelopeError, LinkKey, ENVELOPE_MAX, MAX_DATAGRAM, NO_TOKEN,
};

/// WSAEMSGSIZE: the datagram was larger than the buffer, and is gone.
const WSAEMSGSIZE: i32 = 10040;

/// What the relay has done, counted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayStats {
    /// Client datagrams forwarded to the origin.
    pub up: u64,
    /// Origin datagrams forwarded to a client.
    pub down: u64,
    /// Client datagrams larger than `MAX_DATAGRAM`, dropped.
    pub oversize: u64,
    /// Client datagrams too short to carry a token, dropped.
    pub short: u64,
    /// Client datagrams with a token not issued for their source, dropped.
    pub bad_token: u64,
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
    bad_token: AtomicU64,
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
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<io::Result<()>>>,
}

impl Relay {
    /// Bind the public socket (what clients are told) and the upstream socket
    /// (what the origin allowlists), and start forwarding to `origin`, with
    /// whom this relay shares `key`.
    pub fn spawn(public: impl ToSocketAddrs, upstream: impl ToSocketAddrs, origin: SocketAddr, key: LinkKey) -> io::Result<Self> {
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
            let (c, stop) = (counters.clone(), stop.clone());
            std::thread::spawn(move || forward_up(&public, &upstream, origin, &key, &c, &stop))
        };
        let down = {
            let (c, stop) = (counters.clone(), stop.clone());
            std::thread::spawn(move || forward_down(&upstream, &public, origin, &key, &c, &stop))
        };
        Ok(Self { public: public_addr, upstream: upstream_addr, counters, stop, threads: vec![up, down] })
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
            bad_token: get(&c.bad_token),
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
            let local: SocketAddr = if to.is_ipv4() { (Ipv4Addr::LOCALHOST, 0).into() } else { (Ipv6Addr::LOCALHOST, 0).into() };
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

fn forward_up(
    public: &UdpSocket,
    upstream: &UdpSocket,
    origin: SocketAddr,
    key: &LinkKey,
    c: &Counters,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let got = public.recv_from(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match got {
            Ok((n, client)) => match split_frame(&buf[..n]) {
                None => bump(&c.short),
                Some((token, _)) if token != NO_TOKEN && !token_valid(key, client, token) => bump(&c.bad_token),
                Some(_) => {
                    let _ = upstream.send_to(&wrap(key, Dir::Up, client, &buf[..n]), origin);
                    bump(&c.up);
                }
            },
            Err(e) if e.raw_os_error() == Some(WSAEMSGSIZE) => bump(&c.oversize),
            Err(e) if is_reset(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

fn forward_down(
    upstream: &UdpSocket,
    public: &UdpSocket,
    origin: SocketAddr,
    key: &LinkKey,
    c: &Counters,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut buf = vec![0u8; MAX_DATAGRAM + ENVELOPE_MAX];
    loop {
        let got = upstream.recv_from(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match got {
            Ok((_, from)) if from != origin => bump(&c.foreign),
            Ok((n, _)) => match unwrap(key, Dir::Down, &buf[..n]) {
                Ok((client, payload)) => {
                    let _ = public.send_to(payload, client);
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
    use aegis_protocol::mint_token;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(1);
    const KEY: LinkKey = [5; 16];

    fn sock() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(WAIT)).unwrap();
        s
    }

    fn recv(s: &UdpSocket) -> (Vec<u8>, SocketAddr) {
        let mut buf = vec![0u8; MAX_DATAGRAM + ENVELOPE_MAX];
        let (n, from) = s.recv_from(&mut buf).expect("nothing arrived");
        buf.truncate(n);
        (buf, from)
    }

    /// A client datagram: `token`, then `body`.
    fn tokened(token: u64, body: &[u8]) -> Vec<u8> {
        let mut d = token.to_le_bytes().to_vec();
        d.extend(body);
        d
    }

    /// Wait (bounded) until the relay's counters say `done`.
    fn settle(r: &Relay, done: impl Fn(RelayStats) -> bool) -> RelayStats {
        let t = Instant::now();
        while !done(r.stats()) && t.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(1));
        }
        r.stats()
    }

    /// Both directions: the origin gets the client's datagram wrapped with
    /// the client's address, from the upstream address; the client gets the
    /// origin's reply bare, from the public address — never from the origin.
    #[test]
    fn forwards_both_ways_and_the_client_only_sees_the_relay() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let client = sock();

        let hello = tokened(NO_TOKEN, b"hello");
        client.send_to(&hello, r.public_addr()).unwrap();
        let (got, from) = recv(&origin);
        assert_eq!(from, r.upstream_addr());
        assert_eq!(unwrap(&KEY, Dir::Up, &got), Ok((client.local_addr().unwrap(), &hello[..])));

        origin.send_to(&wrap(&KEY, Dir::Down, client.local_addr().unwrap(), b"back"), r.upstream_addr()).unwrap();
        let (got, from) = recv(&client);
        assert_eq!((got.as_slice(), from), (&b"back"[..], r.public_addr()));
        assert_ne!(from, origin.local_addr().unwrap());
        // Each thread counts after it sends, so the reader can be first.
        assert_eq!(settle(&r, |s| s.up == 1 && s.down == 1), RelayStats { up: 1, down: 1, ..Default::default() });
    }

    /// Only the origin can make the relay send to a client: anyone else
    /// writing to the upstream socket is dropped — otherwise the relay would
    /// reflect whatever a stranger wrapped at whatever address it named.
    #[test]
    fn upstream_ignores_everyone_but_the_origin() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let victim = sock();
        victim.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let stranger = sock();
        stranger.send_to(&wrap(&KEY, Dir::Down, victim.local_addr().unwrap(), b"x"), r.upstream_addr()).unwrap();
        let s = settle(&r, |s| s.foreign == 1);
        assert_eq!(s, RelayStats { foreign: 1, ..Default::default() });
        let mut buf = [0u8; 16];
        assert!(victim.recv(&mut buf).is_err(), "the relay reflected a stranger's datagram");
    }

    #[test]
    fn a_bad_envelope_from_the_origin_is_dropped() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
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
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let victim = sock();
        victim.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let v = victim.local_addr().unwrap();
        origin.send_to(&wrap(&[6; 16], Dir::Down, v, b"flood"), r.upstream_addr()).unwrap();
        origin.send_to(&wrap(&KEY, Dir::Up, v, b"flood"), r.upstream_addr()).unwrap();
        assert_eq!(settle(&r, |s| s.bad_mac == 2), RelayStats { bad_mac: 2, ..Default::default() });
        let mut buf = [0u8; 16];
        assert!(victim.recv(&mut buf).is_err(), "the relay forwarded a forged envelope");
        // under the key, the same thing goes through
        origin.send_to(&wrap(&KEY, Dir::Down, v, b"ok"), r.upstream_addr()).unwrap();
        assert_eq!(recv(&victim).0, b"ok");
    }

    /// Edge: a client datagram of exactly MAX_DATAGRAM is forwarded whole
    /// (its envelope may make it bigger than that — the origin's buffer has
    /// room); one byte over is not forwarded at all.
    #[test]
    fn max_datagram_goes_through_one_byte_more_does_not() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let client = sock();
        client.send_to(&tokened(NO_TOKEN, &[7u8; MAX_DATAGRAM - 8]), r.public_addr()).unwrap();
        let (got, _) = recv(&origin);
        assert_eq!(unwrap(&KEY, Dir::Up, &got).unwrap().1.len(), MAX_DATAGRAM);

        client.send_to(&tokened(NO_TOKEN, &[7u8; MAX_DATAGRAM - 7]), r.public_addr()).unwrap();
        let s = settle(&r, |s| s.oversize + s.up >= 2);
        #[cfg(windows)] // elsewhere it arrives truncated and is forwarded cut
        assert_eq!((s.up, s.oversize), (1, 1));
        let _ = s;
    }

    /// The edge filter at its edges. From one client: a datagram one byte too
    /// short to hold a token, a token minted for another port, a guess, and a
    /// token minted under another key are all dropped at the relay; a token
    /// minted for exactly this address and a token of 0 (a Join) cross.
    #[test]
    fn only_a_token_issued_for_this_address_crosses() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let client = sock();
        let me = client.local_addr().unwrap();
        let other_port = SocketAddr::new(me.ip(), me.port().wrapping_add(1));
        let send = |d: &[u8]| client.send_to(d, r.public_addr()).unwrap();

        send(&[0u8; 7]); // 7 bytes: no token
        send(&tokened(mint_token(&KEY, other_port, 1), b"x"));
        send(&tokened(0x1234_5678_9ABC_DEF0, b"x"));
        send(&tokened(mint_token(&[1; 16], me, 1), b"x"));
        let s = settle(&r, |s| s.short + s.bad_token == 4);
        assert_eq!((s.short, s.bad_token, s.up), (1, 3, 0));

        let good = tokened(mint_token(&KEY, me, 1), b"in");
        send(&good);
        assert_eq!(unwrap(&KEY, Dir::Up, &recv(&origin).0), Ok((me, &good[..])));
        let join = tokened(NO_TOKEN, b"join");
        send(&join);
        assert_eq!(unwrap(&KEY, Dir::Up, &recv(&origin).0), Ok((me, &join[..])));
        // Exactly 8 bytes is a token and an empty body: it crosses (the
        // origin's decoder refuses the body).
        send(&tokened(NO_TOKEN, b""));
        assert_eq!(unwrap(&KEY, Dir::Up, &recv(&origin).0).unwrap().1.len(), 8);
        assert_eq!(settle(&r, |s| s.up == 3).up, 3);
    }

    /// Drop stops both threads promptly.
    #[test]
    fn drop_stops_the_threads() {
        let origin = sock();
        let r = Relay::spawn("127.0.0.1:0", "127.0.0.1:0", origin.local_addr().unwrap(), KEY).unwrap();
        let t = Instant::now();
        drop(r);
        assert!(t.elapsed() < WAIT);
    }
}
