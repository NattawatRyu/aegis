//! Opponents for a live game: the lab's bots, each on its own thread and its
//! own UDP socket, playing through the relay exactly as a player would. Each
//! acts once per new snapshot, on what that snapshot showed it.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use aegis_client_sdk::aimbot::AimbotBot;
use aegis_client_sdk::camper::CamperBot;
use aegis_client_sdk::honest::HonestBot;
use aegis_client_sdk::humanized::HumanizedAimbot;
use aegis_client_sdk::rusher::RusherBot;
use aegis_client_sdk::triggerbot::TriggerBot;
use aegis_client_sdk::{Bot, BotCtx};
use aegis_protocol::{
    decode, frame, open_down, seal_up, ClientKeys, ClientMsg, PlayerId, ServerMsg, EDGE_DOWN_MAX, MAX_DATAGRAM,
    NO_TOKEN,
};

/// How long a bot waits on its socket before it checks whether to stop,
/// and resends its Join if it has not been admitted.
const POLL: Duration = Duration::from_millis(100);

/// A bot a live game can field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotKind {
    /// Walks, turns at walls, fires with a human reaction and a human hand.
    Honest,
    /// Closes in on the nearest enemy.
    Rusher,
    /// Holds its ground.
    Camper,
    /// Fires the tick an enemy comes into sight, dead on.
    Aimbot,
    /// Human aim, machine trigger.
    Trigger,
    /// An aimbot that waits like a person and adds a little noise.
    Humanized,
}

impl BotKind {
    pub fn cheats(self) -> bool {
        matches!(self, Self::Aimbot | Self::Trigger | Self::Humanized)
    }

    fn make(self, seed: u32) -> Box<dyn Bot> {
        match self {
            Self::Honest => Box::new(HonestBot::with_seed(seed)),
            Self::Rusher => Box::new(RusherBot::with_seed(seed)),
            Self::Camper => Box::new(CamperBot::with_seed(seed)),
            Self::Aimbot => Box::new(AimbotBot::new()),
            Self::Trigger => Box::new(TriggerBot::with_seed(seed)),
            Self::Humanized => Box::new(HumanizedAimbot::new()),
        }
    }
}

impl FromStr for BotKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "honest" => Self::Honest,
            "rusher" => Self::Rusher,
            "camper" => Self::Camper,
            "aimbot" => Self::Aimbot,
            "trigger" => Self::Trigger,
            "humanized" => Self::Humanized,
            _ => return Err(format!("unknown bot {s:?} (honest, rusher, camper, aimbot, trigger, humanized)")),
        })
    }
}

/// `honest:3,aimbot` -> three honest bots and one aimbot.
pub fn parse_roster(s: &str) -> Result<Vec<BotKind>, String> {
    let mut out = Vec::new();
    for part in s.split(',').filter(|p| !p.is_empty()) {
        let (kind, n) = match part.split_once(':') {
            Some((k, n)) => (k, n.parse::<usize>().map_err(|_| format!("bad count in {part:?}"))?),
            None => (part, 1),
        };
        out.extend(std::iter::repeat_n(kind.parse::<BotKind>()?, n));
    }
    Ok(out)
}

/// Bot `i` sends from its own loopback IP: the server and the relay budget
/// joins and sessions per IP, as they would real players on real networks.
/// 127.0.0.1 is left to the players.
pub fn bot_ip(i: usize) -> Ipv4Addr {
    Ipv4Addr::new(127, 0, 0, u8::try_from(i + 2).expect("at most 254 bots"))
}

/// A bot playing on its own thread. Stops when dropped.
pub struct BotThread {
    pub kind: BotKind,
    /// Its player id once admitted, else 0.
    id: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl BotThread {
    /// Bot `i` of `kind`, sending to `to`, sealing under `keys` (through a
    /// relay) or not.
    pub fn spawn(i: usize, kind: BotKind, seed: u32, to: SocketAddr, keys: Option<ClientKeys>) -> io::Result<Self> {
        let sock = UdpSocket::bind((bot_ip(i), 0))?;
        sock.set_read_timeout(Some(POLL))?;
        let (id, stop) = (Arc::new(AtomicU8::new(0)), Arc::new(AtomicBool::new(false)));
        let (i2, s2) = (id.clone(), stop.clone());
        let thread = std::thread::spawn(move || play(kind.make(seed), &sock, to, keys.as_ref(), &i2, &s2));
        Ok(Self { kind, id, stop, thread: Some(thread) })
    }

    /// Its player id, once admitted.
    pub fn player_id(&self) -> Option<PlayerId> {
        Some(self.id.load(Ordering::Relaxed)).filter(|&id| id != 0)
    }
}

impl Drop for BotThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn play(
    mut bot: Box<dyn Bot>,
    sock: &UdpSocket,
    to: SocketAddr,
    keys: Option<&ClientKeys>,
    id: &AtomicU8,
    stop: &AtomicBool,
) -> io::Result<()> {
    let send = |d: &[u8]| sock.send_to(&keys.map_or_else(|| d.to_vec(), |k| seal_up(k, d)), to).map(drop);
    let join = |bot: &dyn Bot, cookie: Option<u64>| {
        let mut m = bot.join();
        if let ClientMsg::Join { cookie: c, .. } = &mut m {
            *c = cookie;
        }
        frame(NO_TOKEN, &m)
    };
    let (mut cookie, mut session, mut newest) = (None, None::<(PlayerId, u64)>, 0u32);
    send(&join(&*bot, None))?;
    let mut buf = vec![0u8; MAX_DATAGRAM + EDGE_DOWN_MAX];
    while !stop.load(Ordering::Relaxed) {
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // Not admitted yet: the Join or its answer was lost.
                if session.is_none() {
                    send(&join(&*bot, cookie))?;
                }
                continue;
            }
            // Windows: a send earlier went nowhere. Says nothing about us.
            Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
            Err(e) => return Err(e),
        };
        let body = match keys {
            Some(k) => match open_down(k, &mut buf[..n]) {
                Ok(b) => b,
                Err(_) => continue,
            },
            None => &buf[..n],
        };
        match decode::<ServerMsg>(body) {
            Ok(ServerMsg::Challenge { cookie: c }) => {
                cookie = Some(c);
                send(&join(&*bot, cookie))?;
            }
            Ok(ServerMsg::Joined { player_id, token, .. }) => {
                session = Some((player_id, token));
                id.store(player_id, Ordering::Relaxed);
            }
            Ok(ServerMsg::Snapshot { tick, proof, ack, players }) => {
                bot.landed(tick, proof, &players);
                let Some((my_id, token)) = session else { continue };
                if tick <= newest {
                    continue; // overtaken on the way: already acted on a newer one
                }
                newest = tick;
                bot.acked(ack);
                let ctx = BotCtx { tick, proof, my_id, token, snapshot: &players };
                for (_, d) in bot.routed(&ctx) {
                    send(&d)?;
                }
            }
            Ok(ServerMsg::Event { .. }) | Err(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roster_parses_counts_and_refuses_what_it_does_not_know() {
        use BotKind::*;
        assert_eq!(parse_roster("honest:2,aimbot"), Ok(vec![Honest, Honest, Aimbot]));
        assert_eq!(parse_roster(""), Ok(vec![]));
        assert_eq!(parse_roster("honest:0"), Ok(vec![]));
        assert!(parse_roster("wallhack").is_err());
        assert!(parse_roster("honest:x").is_err());
        assert!([Aimbot, Trigger, Humanized].iter().all(|k| k.cheats()));
        assert!([Honest, Rusher, Camper].iter().all(|k| !k.cheats()));
    }
}
