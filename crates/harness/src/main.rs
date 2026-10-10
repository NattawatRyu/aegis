//! `aegis-harness` — run the standard scenario, print what each defense
//! stopped and what the detector flagged, and write the telemetry to
//! `scenarios/out/<scenario>.jsonl`.
//!
//! `aegis-harness --udp` runs the same scenario over real UDP sockets on
//! loopback and writes `<scenario>.udp.jsonl` — which must be byte-identical
//! to the in-process file. `aegis-harness --relay` does the same with the
//! server as an origin behind a relay and writes `<scenario>.relay.jsonl` —
//! also byte-identical.
//!
//! `aegis-harness sweep [crowds] [rtt]` instead runs that many honest crowds
//! (16 players, every one at round trip `rtt` ticks, default 0) and prints
//! how the honest population scores on every detector signal, over the whole
//! run and at its online peak — the measurement the detector thresholds are
//! set from.
//!
//! `aegis-harness lag [seeds] [max_rtt] [stale]` runs [`Scenario::lag_mix`] (or,
//! with `stale`, [`Scenario::stale_mix`]) crowds at
//! every round trip 0..=max_rtt and prints, per kind of player, how its
//! timed reactions read and how often the reaction detector flagged it.
//!
//! `aegis-harness foresight [seeds] [max_rtt]` runs both mixes and prints,
//! per kind of player, how many of its shots could be held against an enemy
//! only a newer snapshot showed, and how many fit such an enemy under four
//! (window, clear) rules — the measurement `foresight::MIN_FORESEEN` and its
//! radii are set from.
//!
//! `aegis-harness cull [lobbies] [max_lag]` replays honest lobbies and prints,
//! per culling margin, what it leaks and how late a lagging client sees an
//! enemy — the measurement `MAX_MARGIN_TICKS` is set from.
//! `aegis-harness cull-scale [lobbies]` asks what that leak would be in a
//! world where players move less per tick.
//!
//! `aegis-harness scale [crowds]` times the server's share of every tick in
//! honest crowds of 16 to 253 players — how big one room can be inside the
//! 30 Hz budget, and how many rooms one core holds. Run it `--release`.

use std::path::PathBuf;

use aegis_harness::{
    cull_sweep, edge_dropped, edge_seen, honest_sweep_at, leak_by_step, run, run_relay, run_udp, HonestPlayer, Scenario,
};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("sweep") {
        let crowds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(63);
        sweep(crowds, args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("lag") {
        let seeds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
        let stale = args.get(4).map(String::as_str) == Some("stale");
        lag(seeds, args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6), stale);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("foresight") {
        let seeds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
        foresight(seeds, args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("cull") {
        let lobbies = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
        let max_lag = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
        cull(lobbies, max_lag);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("scale") {
        scale(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("cull-scale") {
        cull_scale(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50));
        return Ok(());
    }
    let mode = args.get(1).map(String::as_str);
    let (r, transport, suffix) = match mode {
        Some("--udp") => (run_udp(Scenario::standard())?, "udp loopback", ".udp"),
        Some("--relay") => (run_relay(Scenario::standard())?, "udp loopback via relay", ".relay"),
        _ => (run(Scenario::standard()), "in-process", ""),
    };

    println!("scenario: {}  ticks: {}  transport: {}\n", r.scenario, r.ticks, transport);
    println!(
        "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5} {:>5} {:>8} {:>11}  flags",
        "bot", "joined", "accepted", "anomaly", "rejected", "shots", "hits", "acc", "kills", "max_step", "hidden/wall"
    );
    for b in &r.bots {
        let rejected = if b.totals.rejected.is_empty() {
            "-".to_string()
        } else {
            b.totals.rejected.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
        };
        let flags = if b.alerts.is_empty() {
            "-".to_string()
        } else {
            let f = |a: &aegis_detector::Alert| {
                format!("{} {:.2}/{} @{}", a.flag.reason.label(), a.flag.value, a.flag.samples, a.tick)
            };
            b.alerts.iter().map(f).collect::<Vec<_>>().join(", ")
        };
        println!(
            "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5.2} {:>5} {:>8.3} {:>11}  {}",
            b.name,
            b.joined(),
            b.totals.accepted,
            b.totals.anomalies,
            rejected,
            b.shots,
            b.hits,
            b.accuracy(),
            b.kills,
            b.max_step,
            format!("{}/{}", b.hidden, b.walled),
            flags
        );
    }
    let net = r.net.dropped.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");
    println!("\nno player to pin it on (counters only): {}", if net.is_empty() { "-" } else { &net });
    let t = r.bystander;
    let amp = if t.tx == 0 { 0.0 } else { t.rx as f64 / t.tx as f64 };
    println!("bystander (forged in its name): {} B sent as it, {} B sent to it, amplification {:.2}x", t.tx, t.rx, amp);
    if let Some(e) = r.relay {
        let (seen, cut) = (edge_seen(&e), edge_dropped(&e));
        println!(
            "relay edge: {} client datagrams in, {} forwarded, {} challenged, {} dropped ({:.0}%: bad_seal {}, expired {}, bad_token {}, join_rate {}, bad_cookie {}, short {}, oversize {})",
            seen,
            e.up,
            e.challenged,
            cut,
            100.0 * cut as f64 / seen.max(1) as f64,
            e.bad_seal,
            e.expired,
            e.bad_token,
            e.join_rate,
            e.bad_cookie,
            e.short,
            e.oversize
        );
        let kept = seen - e.up;
        println!(
            "relay edge: {:.0}% of client datagrams never reached the origin",
            100.0 * kept as f64 / seen.max(1) as f64
        );
    }

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/out");
    std::fs::create_dir_all(&dir)?;
    let file = format!("{}{}.jsonl", r.scenario, suffix);
    r.telemetry.save(dir.join(&file))?;
    println!("\n{} records -> scenarios/out/{}", r.telemetry.len(), file);
    Ok(())
}

fn cull(lobbies: u32, max_lag: u32) {
    let s = cull_sweep(lobbies, max_lag);
    let pct = |n: u64, of: u64| 100.0 * n as f64 / of.max(1) as f64;
    println!("honest lobbies: {lobbies}  visible pairs: {}  walled pairs: {}\n", s.visible, s.walled);
    println!("late % (enemy in sight, missing from the snapshot sent `lag` ticks earlier) and leak % (walled, sent anyway)\n");
    print!("{:<8} {:>7}", "margin", "leak%");
    (0..=max_lag).for_each(|l| print!(" {:>7}", format!("lag {l}")));
    println!();
    for k in 0..=max_lag as usize {
        print!("{:<8} {:>7.2}", k, pct(s.leaked[k], s.walled));
        s.late[k].iter().for_each(|&n| print!(" {:>7.2}", pct(n, s.visible)));
        println!();
    }
}

fn cull_scale(lobbies: u32) {
    const STEPS: [f32; 7] = [5.0, 2.5, 1.0, 0.5, 0.25, 0.1, 0.05];
    const MAX_MARGIN: u32 = 6;
    let (walled, leaked) = leak_by_step(lobbies, &STEPS, MAX_MARGIN);
    println!("honest lobbies: {lobbies}  walled pairs: {walled}  (walls 8-10 units wide, 30 Hz)\n");
    println!("leak % of walled pairs, by move step per tick and margin in ticks\n");
    print!("{:<6} {:>7}", "step", "u/s");
    (0..=MAX_MARGIN).for_each(|k| print!(" {:>6}", format!("m{k}")));
    println!();
    for (step, row) in STEPS.iter().zip(&leaked) {
        print!("{:<6} {:>7.1}", step, step * 30.0);
        row.iter().for_each(|&n| print!(" {:>6.2}", 100.0 * n as f64 / walled.max(1) as f64));
        println!();
    }
}

fn scale(crowds: u32) {
    const SIZES: [u32; 5] = [16, 32, 64, 128, aegis_harness::MAX_BOTS];
    let budget = aegis_server::net::TICK.as_secs_f64() * 1e6;
    if cfg!(debug_assertions) {
        println!("WARNING: debug build - timings are meaningless, run with --release\n");
    }
    println!("honest crowds: {crowds} per size, 300 ticks each; server time per tick, bots excluded");
    println!("budget: {budget:.0} us per tick (30 Hz)\n");
    println!(
        "{:>7} {:>6} {:>9} {:>9} {:>9} {:>7} {:>8} {:>10} {:>9}",
        "players", "joined", "p50 us", "p99 us", "max us", "views%", "budget%", "rooms/core", "rec/tick"
    );
    for r in aegis_harness::scale(&SIZES, crowds) {
        let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
        let (p50, p99, max) = (r.at(0.5), r.at(0.99), r.at(1.0));
        let views: f64 = r.ticks.iter().map(|t| us(t.views)).sum();
        let all: f64 = r.ticks.iter().map(|t| us(t.total())).sum();
        println!(
            "{:>7} {:>6} {:>9.1} {:>9.1} {:>9.1} {:>6.0}% {:>7.1}% {:>10.0} {:>9.1}",
            r.players,
            r.joined,
            us(p50.total()),
            us(p99.total()),
            us(max.total()),
            100.0 * views / all.max(1e-9),
            100.0 * us(p99.total()) / budget,
            budget / us(p99.total()).max(1e-9),
            r.records_per_tick
        );
    }
    println!("\nviews% = share of all server time spent building culled snapshots");
    println!("rooms/core = budget / p99 tick: rooms one core runs at 30 Hz, if nothing else ran on it");
}

fn lag(seeds: u32, max_rtt: u32, stale: bool) {
    use aegis_detector::detectors::aim_exact::EXACT_RAD;
    use aegis_detector::detectors::reaction::{is_fast, MIN_TIMED};
    use aegis_detector::FlagReason;
    use aegis_telemetry::Outcome;
    use std::collections::BTreeMap;

    /// One kind of player, summed over every crowd at one round trip.
    #[derive(Default)]
    struct Row {
        players: u32,
        reacts: Vec<u32>,
        judged: u32,
        /// Timed engagements of each player in the row.
        counts: Vec<u32>,
        reaction: u32,
        shots: u32,
        exact: u32,
        aim_exact: u32,
        foresight: u32,
        struck: u32,
        blind: u32,
        any: u32,
    }

    println!(
        "{} crowds: {seeds} per round trip, 12 honest + 4 instant bots, {} ticks",
        if stale { "stale_mix" } else { "lag_mix" },
        aegis_harness::CROWD_TICKS
    );
    println!("react = ticks from the snapshot first showing the enemy to the shot it chose on that picture");
    println!("judged = players with reaction's MIN_TIMED timed engagements; flag columns = players flagged\n");
    println!(
        "{:>3} {:<10} {:>7} {:>6} {:>6} {:>4} {:>4} {:>6} {:>8} {:>7} {:>9} {:>9} {:>4} {:>7}",
        "rtt",
        "player",
        "players",
        "timed",
        "fast%",
        "p10",
        "p50",
        "judged",
        "reaction",
        "exact%",
        "aim_exact",
        "foresight",
        "any",
        "blind%"
    );
    for rtt in 0..=max_rtt {
        let mut rows: BTreeMap<&str, Row> = BTreeMap::new();
        for seed in 0..seeds {
            let r = run(if stale { Scenario::stale_mix(seed, rtt) } else { Scenario::lag_mix(seed, rtt) });
            for b in &r.bots {
                let Some(id) = b.id else { continue };
                let row = rows.entry(b.name).or_default();
                row.players += 1;
                let mut reacts = Vec::new();
                for x in r.telemetry.records().iter().filter(|x| x.player == id) {
                    if let Outcome::Shot { aim_err, react, .. } = x.outcome {
                        row.shots += 1;
                        row.exact += u32::from(aim_err < EXACT_RAD);
                        reacts.extend(react);
                    }
                }
                row.judged += u32::from(reacts.len() as u32 >= MIN_TIMED);
                row.counts.push(reacts.len() as u32);
                row.reacts.extend(reacts);
                let flagged = |why| b.alerts.iter().any(|a| a.flag.reason == why);
                row.reaction += u32::from(flagged(FlagReason::Reaction));
                row.aim_exact += u32::from(flagged(FlagReason::AimExact));
                row.foresight += u32::from(flagged(FlagReason::Foresight));
                row.any += u32::from(!b.alerts.is_empty());
                row.struck += b.struck;
                row.blind += b.blind;
            }
        }
        for (name, mut row) in rows {
            row.reacts.sort_unstable();
            let n = row.reacts.len();
            let q = |p: f32| row.reacts.get(((n.max(1) - 1) as f32 * p).round() as usize).copied().unwrap_or(0);
            let fast = row.reacts.iter().filter(|&&k| is_fast(k)).count();
            println!(
                "{:>3} {:<10} {:>7} {:>6} {:>5.0}% {:>4} {:>4} {:>6} {:>8} {:>6.0}% {:>9} {:>9} {:>4} {:>6.1}%",
                rtt,
                name,
                row.players,
                n,
                100.0 * fast as f32 / n.max(1) as f32,
                q(0.1),
                q(0.5),
                row.judged,
                row.reaction,
                100.0 * row.exact as f32 / row.shots.max(1) as f32,
                row.aim_exact,
                row.foresight,
                row.any,
                100.0 * row.blind as f32 / row.struck.max(1) as f32
            );
            // Players a smaller MIN_TIMED would judge: at 4, 6 and 8.
            let at = |k| row.counts.iter().filter(|&&c| c >= k).count();
            println!(
                "{:>14} timed per player: min {}, judged at 4/6/8: {}/{}/{}",
                "",
                row.counts.iter().min().unwrap_or(&0),
                at(4),
                at(6),
                at(8)
            );
        }
        println!();
    }
}

fn foresight(seeds: u32, max_rtt: u32) {
    use aegis_server::history::Glimpse;
    use std::collections::BTreeMap;

    /// (window, clear) in radians. 0.15 = the honest hand's widest error.
    const GRID: [(f32, f32); 4] = [(0.15, 0.2), (0.15, 0.3), (0.30, 0.3), (0.30, 0.5)];

    println!("lag_mix + stale_mix crowds: {seeds} each per round trip, {} ticks", aegis_harness::CROWD_TICKS);
    println!("open = shots whose aim can be held against an enemy only a newer snapshot showed");
    println!("per (window w, clear c): pooled ahead% of open shots, and [fewest-most] ahead shots per player\n");
    print!("{:>3} {:<10} {:>7} {:>6} {:>9}", "rtt", "player", "players", "open", "open/pl");
    for (w, m) in GRID {
        print!(" {:>20}", format!("w{w:.2} c{m:.2}"));
    }
    println!();
    for rtt in 0..=max_rtt {
        let mut rows: BTreeMap<&str, Vec<Vec<Glimpse>>> = BTreeMap::new();
        for seed in 0..seeds {
            for sc in [Scenario::lag_mix(seed, rtt), Scenario::stale_mix(seed, rtt)] {
                for b in run(sc).bots.into_iter().filter(|b| b.id.is_some()) {
                    rows.entry(b.name).or_default().push(b.glimpses);
                }
            }
        }
        for (name, players) in rows {
            let open: usize = players.iter().map(Vec::len).sum();
            let (lo, hi) =
                (players.iter().map(Vec::len).min().unwrap_or(0), players.iter().map(Vec::len).max().unwrap_or(0));
            print!("{rtt:>3} {name:<10} {:>7} {open:>6} {:>9}", players.len(), format!("{lo}-{hi}"));
            for (w, m) in GRID {
                let per: Vec<usize> =
                    players.iter().map(|g| g.iter().filter(|g| g.ahead <= w && g.claimed > m).count()).collect();
                let all: usize = per.iter().sum();
                let cell = format!(
                    "{:.0}% [{}-{}]",
                    100.0 * all as f32 / open.max(1) as f32,
                    per.iter().min().unwrap_or(&0),
                    per.iter().max().unwrap_or(&0)
                );
                print!(" {cell:>20}");
            }
            let (w, c) = GRID[0];
            let at =
                |k| players.iter().filter(|g| g.iter().filter(|g| g.ahead <= w && g.claimed > c).count() >= k).count();
            println!("  >=2/3/5: {}/{}/{}", at(2), at(3), at(5));
        }
        println!();
    }
}

fn sweep(crowds: u32, rtt: u32) {
    let all = honest_sweep_at(crowds, rtt);
    println!("honest players: {} ({} crowds of {}, rtt {rtt})", all.len(), crowds, aegis_harness::CROWD_SIZE);
    for style in [None, Some("honest"), Some("camper"), Some("rusher")] {
        let players: Vec<&HonestPlayer> = all.iter().filter(|p| style.is_none_or(|s| p.style == s)).collect();
        let col = |f: &dyn Fn(&HonestPlayer) -> f32| {
            let mut v: Vec<f32> = players.iter().map(|p| f(p)).collect();
            v.sort_by(f32::total_cmp);
            let q = |p: f32| v[((v.len() - 1) as f32 * p).round() as usize];
            (q(0.0), q(0.5), q(0.99), q(1.0))
        };
        let ratio = |n: u32, d: u32| if d == 0 { 0.0 } else { n as f32 / d as f32 };
        let rows = [
            ("shots", col(&|p| p.life.shots as f32)),
            ("accuracy", col(&|p| ratio(p.life.hits, p.life.shots))),
            ("aim_exact", col(&|p| ratio(p.life.exact, p.life.shots))),
            ("anomaly_rate", col(&|p| ratio(p.life.anomalies, p.life.accepted))),
            ("timed", col(&|p| p.life.timed as f32)),
            ("reaction", col(&|p| ratio(p.life.fast, p.life.timed))),
            ("accuracy*", col(&|p| p.peak.accuracy)),
            ("aim_exact*", col(&|p| p.peak.aim_exact)),
            ("anomaly_rate*", col(&|p| p.peak.anomaly_rate)),
            ("reaction*", col(&|p| p.peak.reaction)),
        ];
        let flagged = players.iter().filter(|p| !p.alerts.is_empty()).count();

        println!("\n{} ({} players, {flagged} flagged online)", style.unwrap_or("all"), players.len());
        println!("{:<14} {:>8} {:>8} {:>8} {:>8}", "signal", "min", "p50", "p99", "max");
        for (name, (a, b, c, d)) in rows {
            println!("{:<14} {:>8.3} {:>8.3} {:>8.3} {:>8.3}", name, a, b, c, d);
        }
    }
    println!("\n* = online peak: highest value at any record, running lifetime or window, once judged");
}
