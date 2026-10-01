//! `aegis-harness` — run the standard scenario, print what each defense
//! stopped and what the detector flagged, and write the telemetry to
//! `scenarios/out/<scenario>.jsonl`.
//!
//! `aegis-harness --udp` runs the same scenario over real UDP sockets on
//! loopback and writes `<scenario>.udp.jsonl` — which must be byte-identical
//! to the in-process file.
//!
//! `aegis-harness sweep [lobbies]` instead runs that many honest lobbies and
//! prints how the honest population scores on every detector signal — the
//! measurement the detector thresholds are set from.

use std::path::PathBuf;

use aegis_detector::detectors::aim_exact::EXACT_RAD;
use aegis_harness::{honest_sweep, run, run_udp, Scenario};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("sweep") {
        let lobbies = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(250);
        sweep(lobbies);
        return Ok(());
    }
    let udp = args.get(1).map(String::as_str) == Some("--udp");

    let r = if udp { run_udp(Scenario::standard())? } else { run(Scenario::standard()) };

    let transport = if udp { "udp loopback" } else { "in-process" };
    println!("scenario: {}  ticks: {}  transport: {}\n", r.scenario, r.ticks, transport);
    println!(
        "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5} {:>5} {:>8}  flags",
        "bot", "joined", "accepted", "anomaly", "rejected", "shots", "hits", "acc", "kills", "max_step"
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
            "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5.2} {:>5} {:>8.3}  {}",
            b.name, b.joined(), b.totals.accepted, b.totals.anomalies, rejected,
            b.shots, b.hits, b.accuracy(), b.kills, b.max_step, flags
        );
    }
    let net = r.net.dropped.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");
    println!("\nno player to pin it on (counters only): {}", if net.is_empty() { "-" } else { &net });

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/out");
    std::fs::create_dir_all(&dir)?;
    let file = format!("{}{}.jsonl", r.scenario, if udp { ".udp" } else { "" });
    r.telemetry.save(dir.join(&file))?;
    println!("\n{} records -> scenarios/out/{}", r.telemetry.len(), file);
    Ok(())
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
