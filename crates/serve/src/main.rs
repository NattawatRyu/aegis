//! `aegis-serve` — run an Aegis game a client can connect to.
//!
//! ```text
//! aegis-serve [--listen 127.0.0.1:7777] [--bots honest:3,aimbot] [--ticks N]
//!             [--seed N] [--log records.jsonl] [--key-out relay.key] [--direct]
//! ```
//!
//! Prints the address and the relay's public key (and writes the key as hex
//! to `--key-out`, for a client to read), then one line per alert. With
//! `--ticks` it stops after that many ticks and prints each player's tally;
//! without, it runs until killed. `--direct` runs no relay: clients speak to
//! the server unsealed, for debugging only.

use std::fs::File;
use std::io::{self, BufWriter};
use std::net::SocketAddr;

use aegis_serve::bots::{parse_roster, BotKind};
use aegis_serve::{hex, Config, Game};

struct Args {
    cfg: Config,
    ticks: u32,
    log: Option<String>,
    key_out: Option<String>,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        cfg: Config {
            listen: "127.0.0.1:7777".parse().expect("a literal address"),
            direct: false,
            bots: parse_roster("honest:3,aimbot")?,
            seed: 1,
        },
        ticks: 0,
        log: None,
        key_out: None,
    };
    while let Some(flag) = it.next() {
        if flag == "--direct" {
            a.cfg.direct = true;
            continue;
        }
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--listen" => a.cfg.listen = v.parse::<SocketAddr>().map_err(|e| format!("--listen {v}: {e}"))?,
            "--bots" => a.cfg.bots = parse_roster(&v)?,
            "--ticks" => a.ticks = v.parse().map_err(|_| format!("--ticks {v}: not a number"))?,
            "--seed" => a.cfg.seed = v.parse().map_err(|_| format!("--seed {v}: not a number"))?,
            "--log" => a.log = Some(v),
            "--key-out" => a.key_out = Some(v),
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(a)
}

fn main() -> io::Result<()> {
    let args = match parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("aegis-serve: {e}");
            std::process::exit(2);
        }
    };
    let mut game = Game::start(&args.cfg)?;
    if let Some(path) = &args.log {
        game.log_to(BufWriter::new(File::create(path)?));
    }
    match game.relay_public() {
        Some(key) => {
            println!("listening {} relay-key {}", game.public_addr(), hex(&key));
            if let Some(path) = &args.key_out {
                std::fs::write(path, hex(&key))?;
            }
        }
        None => println!("listening {} direct (unsealed)", game.public_addr()),
    }

    while args.ticks == 0 || game.tick_number() < args.ticks {
        for a in game.tick()? {
            let who = game.bots().find(|&(_, id)| id == Some(a.flag.player)).map_or("player", |(k, _)| name(k));
            println!(
                "alert tick={} player={} ({who}) reason={} value={:.3} samples={}",
                a.tick,
                a.flag.player,
                a.flag.reason.label(),
                a.flag.value,
                a.flag.samples
            );
        }
    }

    println!("\n{:<7} {:<10} {:>8} {:>8} {:>5} {:>5}  flags", "player", "who", "accepted", "rejected", "shots", "hits");
    for (&id, t) in game.tallies() {
        let who = game.bots().find(|&(_, b)| b == Some(id)).map_or("player", |(k, _)| name(k));
        let flags = if t.flags.is_empty() { "-".to_string() } else { t.flags.join(",") };
        println!("{id:<7} {who:<10} {:>8} {:>8} {:>5} {:>5}  {flags}", t.accepted, t.rejected, t.shots, t.hits);
    }
    Ok(())
}

fn name(k: BotKind) -> &'static str {
    match k {
        BotKind::Honest => "honest",
        BotKind::Rusher => "rusher",
        BotKind::Camper => "camper",
        BotKind::Aimbot => "aimbot",
        BotKind::Trigger => "trigger",
        BotKind::Humanized => "humanized",
    }
}
