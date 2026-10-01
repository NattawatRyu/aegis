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
//! Known gap, closed in D3: identity is the source address, and UDP source
//! addresses can be forged. Until the session token, a spoofer who knows a
//! player's address can send inputs as that player.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use aegis_protocol::{encode, ServerMsg, TICK_HZ};

use crate::{Server, TickOutcome};

/// One tick of wall-clock time at [`TICK_HZ`].
pub const TICK: Duration = Duration::from_nanos(1_000_000_000 / TICK_HZ as u64);

/// Receive buffer. Every legal client message is far smaller; anything that
/// does not fit is not a client message.
pub const MAX_DATAGRAM: usize = 2048;

/// [`crate::NetStats`] label for a datagram larger than [`MAX_DATAGRAM`]
/// (Windows reports these as an error instead of truncating them).
pub const OVERSIZE: &str = "oversize";

/// WSAEMSGSIZE: the datagram was larger than the buffer, and is gone.
const WSAEMSGSIZE: i32 = 10040;

pub struct NetServer {
    server: Server,
    sock: UdpSocket,
    tick: u32,
    buf: Vec<u8>,
}

impl NetServer {
    pub fn bind(addr: impl ToSocketAddrs, server: Server) -> io::Result<Self> {
        Ok(Self { server, sock: UdpSocket::bind(addr)?, tick: 0, buf: vec![0; MAX_DATAGRAM] })
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

    /// Advance to the next tick and send its snapshot to every admitted
    /// address. A send that fails is that client's problem, not the tick's.
    pub fn begin_tick(&mut self) -> io::Result<()> {
        self.tick += 1;
        let snap = encode(&ServerMsg::Snapshot { tick: self.tick, players: self.server.begin_tick() });
        for (to, _) in self.server.peers() {
            let _ = self.sock.send_to(&snap, to);
        }
        Ok(())
    }

    /// Wait up to `wait` for one datagram and feed it to the server. Returns
    /// whether a datagram was consumed.
    ///
    /// Errors that say something about one earlier packet rather than about
    /// the socket are skipped, not returned: on Windows a datagram sent to a
    /// client that has gone away makes a later `recv_from` fail with
    /// ConnectionReset, and one client leaving must not stop the server.
    pub fn recv_one(&mut self, wait: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            // A zero read timeout is an error, not "don't block".
            self.sock.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            match self.sock.recv_from(&mut self.buf) {
                Ok((n, from)) => {
                    if let Some(player_id) = self.server.receive(self.tick, from, &self.buf[..n]) {
                        let joined = encode(&ServerMsg::Joined { player_id, tick: self.tick });
                        let _ = self.sock.send_to(&joined, from);
                    }
                    return Ok(true);
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(false),
                Err(e) if e.raw_os_error() == Some(WSAEMSGSIZE) => {
                    self.server.count_drop(OVERSIZE);
                    return Ok(true);
                }
                Err(e) if e.kind() == ErrorKind::ConnectionReset => {
                    if left.is_zero() {
                        return Ok(false);
                    }
                }
                Err(e) => return Err(e),
            }
        }
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
    use aegis_protocol::{decode, ClientMsg, Vec2, PROTOCOL_VERSION};

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
        encode(&ClientMsg::Join { name: "t".into(), protocol: PROTOCOL_VERSION })
    }

    fn input(seq: u32) -> Vec<u8> {
        encode(&ClientMsg::Input { seq, tick: seq, move_dir: Vec2::new(1.0, 0.0), aim: Vec2::new(1.0, 0.0), shoot: false })
    }

    fn read(c: &UdpSocket) -> ServerMsg {
        let mut buf = [0u8; MAX_DATAGRAM];
        let n = c.recv(&mut buf).expect("no reply from server");
        decode(&buf[..n]).unwrap()
    }

    #[test]
    fn join_is_answered_and_snapshots_follow() {
        let mut n = net();
        let c = client();
        c.send_to(&join(), n.local_addr().unwrap()).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(read(&c), ServerMsg::Joined { player_id: 1, tick: 0 });

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
        let to = n.local_addr().unwrap();
        let gone = client();
        gone.send_to(&join(), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        drop(gone);
        n.begin_tick().unwrap(); // snapshot into a closed port

        let c = client();
        c.send_to(&join(), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(read(&c), ServerMsg::Joined { player_id: 2, tick: 1 });
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
        c.send_to(&join(), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        assert_eq!(read(&c), ServerMsg::Joined { player_id: 1, tick: 0 });
    }

    #[test]
    fn run_tick_applies_an_input_and_keeps_time() {
        let mut n = net();
        let to = n.local_addr().unwrap();
        let c = client();
        c.send_to(&join(), to).unwrap();
        assert!(n.recv_one(WAIT).unwrap());
        read(&c);
        c.send_to(&input(1), to).unwrap(); // already queued when tick 1 opens

        let t = Instant::now();
        let out = n.run_tick().unwrap();
        let took = t.elapsed();
        assert_eq!(out.steps, vec![(1, crate::sim::MOVE_SPEED)]);
        assert!(took >= TICK, "tick ended early: {took:?}");
        // Loose upper bound: a busy CI box can overshoot, but not by 10 ticks.
        assert!(took < TICK * 10, "tick overran: {took:?}");
    }
}
