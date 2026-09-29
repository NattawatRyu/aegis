//! `aegis-harness` — run the standard scenario, print what each defense
//! stopped, and write the telemetry to `scenarios/out/<scenario>.jsonl`.

use std::path::PathBuf;

use aegis_harness::{run, Scenario};

fn main() -> std::io::Result<()> {
    let r = run(Scenario::standard());

    println!("scenario: {}  ticks: {}\n", r.scenario, r.ticks);
    println!(
        "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5} {:>5} {:>8}",
        "bot", "joined", "accepted", "anomaly", "rejected", "shots", "hits", "acc", "kills", "max_step"
    );
    for b in &r.bots {
        let rejected = if b.totals.rejected.is_empty() {
            "-".to_string()
        } else {
            b.totals.rejected.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
        };
        println!(
            "{:<11} {:>6} {:>8} {:>7}  {:<32} {:>5} {:>5} {:>5.2} {:>5} {:>8.3}",
            b.name, b.joined, b.totals.accepted, b.totals.anomalies, rejected,
            b.shots, b.hits, b.accuracy(), b.kills, b.max_step
        );
    }

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/out");
    std::fs::create_dir_all(&dir)?;
    let file = format!("{}.jsonl", r.scenario);
    r.telemetry.save(dir.join(&file))?;
    println!("\n{} records -> scenarios/out/{}", r.telemetry.len(), file);
    Ok(())
}
