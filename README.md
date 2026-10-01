# Aegis

Open-source toolkit that helps game developers build multiplayer games that
resist **cheating** and **DDoS** from day one — by fixing the *architecture*,
not by chasing individual cheats.

> Status: **pre-alpha.** Step 1 (Lab / Testbed) and Step 2 (detector v0,
> offline rules) done; Step 3 (pillar D) in progress — the server runs over
> real UDP; every client datagram carries a session token checked before
> decode, so a forged source address can neither act as a player nor spend
> its rate budget. Sessions are capped per IP and end after 5 s idle, so one
> machine cannot fill the server. No encryption: an on-path attacker can
> still read tokens.
> Nothing here is production-ready.

## Why

Most cheats and room-DDoS exist because the game trusts the player's machine or
exposes it directly:

- The client is told things it should not see (enemy positions behind a wall) →
  **wallhack / ESP**.
- The server believes what the client claims (position, hit, speed) →
  **teleport / speedhack / instant-hit**.
- Players connect directly to each other or to a bare server IP → **room DDoS**.

Aegis attacks the root cause: the server owns the truth, the client sees only
what it should, and no one sees the real server IP.

## The four pillars

| Pillar | What | Tier |
|--------|------|------|
| **A. Lab / Testbed** | Tiny multiplayer game + attack simulators you run against *yourself* | Free (Apache-2.0) |
| **B. Netcode reference** | Server-authoritative pattern: client sends intent, server decides | Free (Apache-2.0) |
| **C. Anomaly detector** | Server-side telemetry analysis to catch aimbots / closet cheaters | Managed service |
| **D. DDoS-resist patterns** | Relay + session token + edge rate-limit | Free patterns; managed relay hosting |

Open-core: the toolkit is free and permissive so it can become a standard. The
hosted, ongoing-cost pieces (managed detection, managed relay, support) fund the
project.

## Ethics & legal

Aegis is a **defensive** project. Every attack simulator here targets a server
**you run yourself**. Do not point any tool in this repo at a system you do not
own or are not explicitly authorized (bug bounty scope) to test — that is
illegal regardless of intent.

## Layout

```
crates/
  protocol/     wire types — the intent-only contract every crate depends on
  server/       authoritative sim + guards (one defense per file in guards/),
                and the UDP loop (net.rs)
  client-sdk/   programmable bots: honest + one cheat per file
  telemetry/    guard verdicts + shot evidence as jsonl, for the detector
  detector/     pillar C: one detector per file, flags for human review
  harness/      runs every bot against the real guards + sim, reports per bot
scenarios/out/  harness telemetry output (gitignored)
```

Run the lab:

```
cargo run -p aegis-harness            # every bot vs guards + detector
cargo run -p aegis-harness -- --udp   # same, over real UDP on loopback
cargo run -p aegis-harness -- sweep   # honest population per detector signal
```

The UDP run must write telemetry byte-identical to the in-process run:

```
cmp scenarios/out/standard.jsonl scenarios/out/standard.udp.jsonl
```

## License

Apache-2.0. See `LICENSE`.
