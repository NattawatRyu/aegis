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
//! `aegis-harness sweep [lobbies]` instead runs that many honest lobbies and
//! prints how the honest population scores on every detector signal — the
//! measurement the detector thresholds are set from.
//!
//! `aegis-harness cull [lobbies] [max_lag]` replays honest lobbies and prints,
//! per culling margin, what it leaks and how late a lagging client sees an
//! enemy — the measurement `MAX_MARGIN_TICKS` is set from.
//! `aegis-harness cull-scale [lobbies]` asks what that leak would be in a
//! world where players move less per tick.

use std::path::PathBuf;

use aegis_detector::detectors::aim_exact::EXACT_RAD;
use aegis_harness::{cull_sweep, edge_dropped, edge_seen, honest_sweep, leak_by_step, run, run_relay, run_udp, Scenario};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("sweep") {
        let lobbies = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(250);
        sweep(lobbies);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("cull") {
        let lobbies = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
        let max_lag = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
        cull(lobbies, max_lag);
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
        let flags = if b.flags.is_empty() {
            "-".to_string()
        } else {
            b.flags.iter().map(|f| format!("{} {:.2}", f.reason.label(), f.value)).collect::<Vec<_>>().join(", ")
        };
        println!(
            "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5.2} {:>5} {:>8.3} {:>11}  {}",
            b.name, b.joined(), b.totals.accepted, b.totals.anomalies, rejected,
            b.shots, b.hits, b.accuracy(), b.kills, b.max_step, format!("{}/{}", b.hidden, b.walled), flags
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
            "relay edge: {} client datagrams in, {} forwarded, {} challenged, {} dropped ({:.0}%: bad_token {}, join_rate {}, bad_cookie {}, short {}, oversize {})",
            seen,
            e.up,
            e.challenged,
            cut,
            100.0 * cut as f64 / seen.max(1) as f64,
            e.bad_token,
            e.join_rate,
            e.bad_cookie,
            e.short,
            e.oversize
        );
        let kept = seen - e.up;
        println!("relay edge: {:.0}% of client datagrams never reached the origin", 100.0 * kept as f64 / seen.max(1) as f64);
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

fn sweep(lobbies: u32) {
    let players = honest_sweep(lobbies);
    let col = |f: &dyn Fn(&aegis_detector::PlayerStats) -> f32| {
        let mut v: Vec<f32> = players.iter().map(f).collect();
        v.sort_by(f32::total_cmp);
        let q = |p: f32| v[((v.len() - 1) as f32 * p).round() as usize];
        (q(0.0), q(0.5), q(0.99), q(1.0))
    };
    let acc = col(&|s| if s.shots() == 0 { 0.0 } else { s.hits as f32 / s.shots() as f32 });
    let exact = col(&|s| {
        if s.shots() == 0 { 0.0 } else { s.aim_errs.iter().filter(|&&e| e < EXACT_RAD).count() as f32 / s.shots() as f32 }
    });
    let anom = col(&|s| if s.accepted == 0 { 0.0 } else { s.anomalies as f32 / s.accepted as f32 });
    let shots = col(&|s| s.shots() as f32);

    println!("honest players: {} ({} lobbies)\n", players.len(), lobbies);
    println!("{:<14} {:>8} {:>8} {:>8} {:>8}", "signal", "min", "p50", "p99", "max");
    for (name, (a, b, c, d)) in [("shots", shots), ("accuracy", acc), ("aim_exact", exact), ("anomaly_rate", anom)] {
        println!("{:<14} {:>8.3} {:>8.3} {:>8.3} {:>8.3}", name, a, b, c, d);
    }
}
