//! The client ABI against the real thing: an `aegis_client_*` client, driven
//! the way an engine would drive it (its own socket, bytes in and out),
//! joins a real origin through a real relay over UDP — challenged at the
//! edge, admitted, sent snapshots — and every input it sends is accepted:
//! the token, the sealing and the snapshot proofs all check out at the
//! server. Its keys come from the game's backend, or (feature `noise`) from
//! the relay itself over `aegis_noise_*`.

use std::net::{Ipv4Addr, UdpSocket};
use std::ptr::null_mut;
use std::time::Duration;

use aegis_ffi::client::*;
use aegis_ffi::AEGIS_OK;
use aegis_protocol::movement::Wall;
use aegis_protocol::{mint_connect, EdgeKey, LinkKey, Vec2, EDGE_DOWN_MAX, MAX_DATAGRAM};
use aegis_relay::{Clock, Relay};
use aegis_server::{NetServer, Server};
use aegis_telemetry::Outcome;

const WAIT: Duration = Duration::from_secs(2);

/// Read datagrams from the relay until one is of `kind`.
fn until(c: *mut AegisClient, sock: &UdpSocket, kind: u8) -> AegisReceived {
    let mut buf = [0u8; MAX_DATAGRAM + EDGE_DOWN_MAX];
    loop {
        let n = sock.recv(&mut buf).expect("a datagram from the relay");
        let mut rx = AegisReceived::default();
        assert_eq!(aegis_client_receive(c, buf.as_ptr(), n, &mut rx), AEGIS_OK);
        if rx.kind == kind {
            return rx;
        }
    }
}

/// An origin behind a lockstep relay under `edge`.
fn origin_behind_relay(edge: EdgeKey) -> (NetServer, Relay) {
    let mut net = NetServer::bind((Ipv4Addr::LOCALHOST, 0), Server::new(vec![Vec2::ZERO])).unwrap();
    let link = LinkKey::random();
    let relay = Relay::spawn_with(
        (Ipv4Addr::LOCALHOST, 0),
        (Ipv4Addr::LOCALHOST, 0),
        net.local_addr().unwrap(),
        link,
        edge,
        Clock::lockstep(),
    )
    .unwrap();
    net.behind_relay(relay.upstream_addr(), link);
    (net, relay)
}

fn relay_socket(relay: &Relay) -> UdpSocket {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    sock.set_read_timeout(Some(WAIT)).unwrap();
    sock.connect(relay.public_addr()).unwrap();
    sock
}

/// A C client with `keys`, on `sock`: joins, plays TICKS ticks, and every
/// input is accepted.
fn joins_and_every_input_is_accepted(net: &mut NetServer, relay: &Relay, sock: &UdpSocket, keys: &[u8]) {
    let mut c = null_mut();
    assert_eq!(aegis_client_new(c"riw".as_ptr(), keys.as_ptr(), &mut c), AEGIS_OK);
    let mut buf = [0u8; AEGIS_SEND_MAX];
    let mut send_join = |c| {
        let n = aegis_client_join(c, buf.as_mut_ptr(), buf.len());
        assert!(n > 0);
        sock.send(&buf[..n as usize]).unwrap();
    };

    send_join(c);
    until(c, sock, AEGIS_RX_CHALLENGE);
    send_join(c);
    assert!(net.recv_one(WAIT).unwrap(), "the answered Join crossed");
    let joined = until(c, sock, AEGIS_RX_JOINED);
    assert_eq!(aegis_client_player_id(c), i32::from(joined.player));

    const TICKS: u32 = 6;
    for _ in 0..TICKS {
        net.begin_tick().unwrap();
        relay.next_window();
        let snap = until(c, sock, AEGIS_RX_SNAPSHOT);
        assert_eq!(snap.tick, net.tick());
        assert_eq!(aegis_client_player_count(c), 1, "alone, it sees itself");
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let n = aegis_client_input(c, AEGIS_TICK_NEWEST, 1.0, 0.0, 1.0, 0.0, false, buf.as_mut_ptr(), buf.len());
        assert!(n > 0, "{n}");
        sock.send(&buf[..n as usize]).unwrap();
        assert!(net.recv_one(WAIT).unwrap());
        net.end_tick();
    }

    let records = net.server().telemetry().records();
    let mine = |o: fn(&Outcome) -> bool| records.iter().filter(|r| r.player == joined.player && o(&r.outcome)).count();
    assert_eq!(mine(|o| matches!(o, Outcome::Accepted { .. })), TICKS as usize);
    assert_eq!(mine(|o| matches!(o, Outcome::Rejected { .. })), 0, "{records:?}");
    let mut me = AegisPlayer::default();
    assert_eq!(aegis_client_player(c, 0, &mut me), AEGIS_OK);
    assert!(me.alive != 0 && me.x > 0.0, "it walked: {me:?}");
    aegis_client_free(c);
}

#[test]
fn a_c_client_joins_through_a_relay_and_every_input_is_accepted() {
    let edge = EdgeKey::random();
    let (mut net, relay) = origin_behind_relay(edge);
    // The backend's part, over HTTPS in a real deployment.
    let keys = mint_connect(&edge, u64::MAX).to_bytes();
    joins_and_every_input_is_accepted(&mut net, &relay, &relay_socket(&relay), &keys);
}

/// A C client that draws behind the newest snapshot and predicts itself,
/// against a real origin with a wall in its path: two frames a tick, each
/// half a tick, so most land between snapshots. Every input is accepted
/// (claims and offsets the server takes), some carry an offset, and every
/// prediction checked is the server's to the bit — the wall crossed the ABI.
#[test]
fn an_interpolating_c_client_claims_what_it_draws_and_predicts_exactly() {
    let edge = EdgeKey::random();
    let wall = Wall::new(Vec2::new(20.0, -5.0), Vec2::new(22.0, 5.0));
    let mut net = NetServer::bind((Ipv4Addr::LOCALHOST, 0), Server::with_walls(vec![Vec2::ZERO], &[wall])).unwrap();
    let link = LinkKey::random();
    let relay = Relay::spawn_with(
        (Ipv4Addr::LOCALHOST, 0),
        (Ipv4Addr::LOCALHOST, 0),
        net.local_addr().unwrap(),
        link,
        edge,
        Clock::lockstep(),
    )
    .unwrap();
    net.behind_relay(relay.upstream_addr(), link);
    let sock = relay_socket(&relay);
    let keys = mint_connect(&edge, u64::MAX).to_bytes();

    let mut c = null_mut();
    assert_eq!(aegis_client_new(c"riw".as_ptr(), keys.as_ptr(), &mut c), AEGIS_OK);
    assert_eq!(aegis_client_interpolate(c, 1.0), AEGIS_OK);
    let w = AegisWall { min_x: wall.min.x, min_y: wall.min.y, max_x: wall.max.x, max_y: wall.max.y };
    assert_eq!(aegis_client_predict(c, &w, 1), AEGIS_OK);
    let mut buf = [0u8; AEGIS_SEND_MAX];
    let mut send_join = |c| {
        let n = aegis_client_join(c, buf.as_mut_ptr(), buf.len());
        sock.send(&buf[..n as usize]).unwrap();
    };
    send_join(c);
    until(c, &sock, AEGIS_RX_CHALLENGE);
    send_join(c);
    assert!(net.recv_one(WAIT).unwrap());
    let joined = until(c, &sock, AEGIS_RX_JOINED);

    // Walking east at 5 a tick, it reaches the wall at x 20 by tick 4.
    const TICKS: u32 = 12;
    let mut offsets = Vec::new();
    for _ in 0..TICKS {
        net.begin_tick().unwrap();
        relay.next_window();
        until(c, &sock, AEGIS_RX_SNAPSHOT);
        let mut v = AegisView::default();
        for _ in 0..2 {
            assert_eq!(aegis_client_view(c, 0.5, &mut v), AEGIS_OK);
        }
        offsets.push(v.offset);
        let mut buf = [0u8; AEGIS_SEND_MAX];
        let n = aegis_client_input_drawn(c, 1.0, 0.0, 1.0, 0.0, false, buf.as_mut_ptr(), buf.len());
        assert!(n > 0, "{n}");
        sock.send(&buf[..n as usize]).unwrap();
        assert!(net.recv_one(WAIT).unwrap());
        net.end_tick();
    }

    let records = net.server().telemetry().records();
    let mine = |o: fn(&Outcome) -> bool| records.iter().filter(|r| r.player == joined.player && o(&r.outcome)).count();
    assert_eq!(mine(|o| matches!(o, Outcome::Accepted { .. })), TICKS as usize);
    assert_eq!(mine(|o| matches!(o, Outcome::Rejected { .. })), 0, "{records:?}");
    assert!(offsets.iter().any(|&o| o < 0), "some frames sat between snapshots: {offsets:?}");

    let mut st = AegisPredictStats::default();
    assert_eq!(aegis_client_prediction_stats(c, &mut st), AEGIS_OK);
    assert!(st.checked >= TICKS - 2 && st.exact == st.checked, "{st:?}");
    let (mut x, mut y) = (0f32, 0f32);
    assert_eq!(aegis_client_predicted(c, &mut x, &mut y), AEGIS_OK);
    assert!(x < wall.min.x, "stopped by the wall: {x}");
    let mut pace = AegisPace::default();
    assert_eq!(aegis_client_pace(c, &mut pace), AEGIS_OK);
    assert_eq!(pace.frames, 2 * TICKS - 1);
    aegis_client_free(c);
}

/// No backend: the C client knows only the relay's public key, is
/// challenged, comes back with the cookie, is welcomed with keys under the
/// relay's current epoch, and then plays like any other client.
#[cfg(feature = "noise")]
#[test]
fn a_c_client_with_keys_from_a_noise_relay_joins_and_every_input_is_accepted() {
    use aegis_ffi::noise::*;
    use aegis_protocol::noise::RelayStatic;

    let (mut net, relay) = origin_behind_relay(EdgeKey::with_epoch([1; 32], 1));
    relay.add_edge(EdgeKey::with_epoch([6; 32], 6));
    let me = RelayStatic::generate();
    relay.enable_noise(me.clone(), 60);
    let sock = relay_socket(&relay);

    let handshake = |cookie: u64| -> (i32, u64, [u8; AEGIS_CLIENT_KEYS_LEN]) {
        let mut n = null_mut();
        let mut buf = [0u8; AEGIS_NOISE_HELLO_LEN];
        let len = aegis_noise_hello(me.public().as_ptr(), cookie, &mut n, buf.as_mut_ptr(), buf.len());
        assert_eq!(len, AEGIS_NOISE_HELLO_LEN as i32);
        sock.send(&buf).unwrap();
        let mut reply = [0u8; 512];
        let got = sock.recv(&mut reply).expect("an answer from the relay");
        let (mut cookie, mut keys) = (0, [0u8; AEGIS_CLIENT_KEYS_LEN]);
        let s = aegis_noise_answer(n, reply.as_ptr(), got, &mut cookie, keys.as_mut_ptr());
        aegis_noise_free(n);
        (s, cookie, keys)
    };
    let (s, cookie, _) = handshake(0);
    assert_eq!(s, AEGIS_NOISE_CHALLENGE);
    let (s, _, keys) = handshake(cookie);
    assert_eq!(s, AEGIS_NOISE_WELCOME);
    let sid = aegis_protocol::ClientKeys::from_bytes(&keys).sid;
    assert_eq!(sid.epoch(), 6, "minted under the epoch added last");
    joins_and_every_input_is_accepted(&mut net, &relay, &sock, &keys);
}
