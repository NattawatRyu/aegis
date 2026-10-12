//! A live game against the clock, as an engine's client meets it: a client
//! that knows only the relay's address and public key runs the Noise
//! handshake, joins, draws behind the newest snapshot, predicts itself, and
//! sends an input on every snapshot — while bots, one of them an aimbot,
//! play through the same relay.
//!
//! Real time on a shared machine jitters, so the bars are shares, not exact
//! counts.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aegis_client_sdk::conn::{Connection, Received};
use aegis_client_sdk::interp::Pace;
use aegis_client_sdk::predict::PredictStats;
use aegis_protocol::noise::{hello, Answer};
use aegis_protocol::{PlayerId, Vec2, EDGE_DOWN_MAX, MAX_DATAGRAM};
use aegis_serve::bots::{parse_roster, BotKind};
use aegis_serve::{Config, Game};
use aegis_server::ARENA_WALLS;

/// Ten seconds of play.
const TICKS: u32 = 300;

/// What the client saw of its own game.
struct Probe {
    id: PlayerId,
    sent: u32,
    /// Of those, sent while the newest snapshot showed it alive.
    alive: u32,
    prediction: PredictStats,
    pace: Pace,
}

/// Wait up to `wait` for one datagram on a non-blocking socket. Non-blocking
/// rather than a read timeout: on Windows a UDP read that times out while a
/// datagram arrives can lose it.
fn recv(sock: &UdpSocket, buf: &mut [u8], wait: Duration) -> Option<usize> {
    let t = Instant::now();
    loop {
        match sock.recv(buf) {
            Ok(n) => return Some(n),
            Err(_) if t.elapsed() < wait => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => return None,
        }
    }
}

fn play(to: SocketAddr, relay_key: [u8; 32], stop: &AtomicBool) -> Probe {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.connect(to).unwrap();
    sock.set_nonblocking(true).unwrap();
    let mut buf = vec![0u8; MAX_DATAGRAM + EDGE_DOWN_MAX];

    // Keys from the relay itself: challenged, then welcomed.
    let (mut h, wire) = hello(&relay_key, None);
    sock.send(&wire).unwrap();
    let n = recv(&sock, &mut buf, Duration::from_secs(2)).expect("no challenge");
    let Ok(Answer::Challenge(cookie)) = h.answer(&buf[..n]) else { panic!("not a challenge") };
    let (mut h, wire) = hello(&relay_key, Some(cookie));
    sock.send(&wire).unwrap();
    let n = recv(&sock, &mut buf, Duration::from_secs(2)).expect("no welcome");
    let Ok(Answer::Welcome(keys)) = h.answer(&buf[..n]) else { panic!("not a welcome") };

    let mut c = Connection::new("probe", Some(keys));
    c.interpolate(1.0);
    c.predict_in(&ARENA_WALLS);
    let id = loop {
        sock.send(&c.join()).unwrap();
        if let Some(n) = recv(&sock, &mut buf, Duration::from_millis(300)) {
            if let Ok(Received::Joined { player_id, .. }) = c.receive(&mut buf[..n]) {
                break player_id;
            }
        }
        assert!(!stop.load(Ordering::Relaxed), "never admitted");
    };

    // A frame every few ms, as an engine draws; an input per new snapshot,
    // chosen on the frame on screen. It walks a square and never shoots.
    let (mut sent, mut alive, mut acted, mut last) = (0u32, 0u32, 0u32, Instant::now());
    let dirs = [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0), Vec2::new(-1.0, 0.0), Vec2::new(0.0, -1.0)];
    while !stop.load(Ordering::Relaxed) {
        while let Ok(n) = sock.recv(&mut buf) {
            let _ = c.receive(&mut buf[..n]);
        }
        let dt = last.elapsed().as_secs_f64() * f64::from(aegis_protocol::TICK_HZ);
        last = Instant::now();
        let Some(view) = c.view(dt) else { continue };
        if c.newest_tick().is_some_and(|t| t > acted) {
            acted = c.newest_tick().unwrap();
            let walk = dirs[(sent / 20) as usize % 4];
            let d = c.input_drawn(&view, walk, Vec2::new(1.0, 0.0), false).expect("an input on the frame drawn");
            sock.send(&d).unwrap();
            sent += 1;
            alive += u32::from(c.players().iter().any(|p| p.id == id && p.alive));
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Probe { id, sent, alive, prediction: c.prediction_stats().unwrap(), pace: c.pace().unwrap() }
}

#[test]
fn a_client_with_only_the_relays_key_plays_a_live_game_and_the_aimbot_is_caught() {
    let cfg = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        direct: false,
        bots: parse_roster("honest:3,aimbot").unwrap(),
        seed: 1,
    };
    let mut game = Game::start(&cfg).unwrap();
    let (to, key) = (game.public_addr(), game.relay_public().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let client = {
        let stop = stop.clone();
        std::thread::spawn(move || play(to, key, &stop))
    };
    for _ in 0..TICKS {
        game.tick().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let p = client.join().unwrap();

    assert!(game.server().telemetry().is_empty(), "records kept after they were streamed");
    let me = game.tallies().get(&p.id).cloned().expect("the client has a session");
    assert!(p.sent > TICKS * 8 / 10, "it acted on {} of {TICKS} ticks", p.sent);
    assert!(me.accepted * 100 >= p.sent * 95, "accepted {} of {} sent: {me:?}", me.accepted, p.sent);
    assert!(me.rejected * 100 <= p.sent, "{me:?}");
    assert!(me.flags.is_empty(), "the client was flagged: {me:?}");
    let s = p.prediction;
    // Only a life is predicted: the aimbot keeps it short. Of the inputs
    // sent alive, most are checked (the last of each life is acked dead),
    // and the checked are the server's to the bit.
    let why = format!("{s:?} over {} sent, {} alive", p.sent, p.alive);
    assert!(s.checked >= 20 && s.checked * 10 >= p.alive * 6, "{why}");
    assert!(s.exact * 100 >= s.checked * 99, "{why}");
    assert!(p.pace.frames > p.sent, "{:?}", p.pace);

    for (kind, id) in game.bots() {
        let id = id.unwrap_or_else(|| panic!("{kind:?} never admitted"));
        let t = &game.tallies()[&id];
        match kind {
            BotKind::Aimbot => assert!(t.flags.contains(&"aim_exact"), "aimbot missed: {t:?}"),
            _ => assert!(t.flags.is_empty(), "{kind:?} flagged: {t:?}"),
        }
    }
}
