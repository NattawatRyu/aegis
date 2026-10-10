# Aegis

[![ci](https://github.com/NattawatRyu/aegis/actions/workflows/ci.yml/badge.svg)](https://github.com/NattawatRyu/aegis/actions/workflows/ci.yml)

Open-source toolkit that helps game developers build multiplayer games that
resist **cheating** and **DDoS** from day one — by fixing the *architecture*,
not by chasing individual cheats.

> Status: **pre-alpha.** Step 1 (Lab / Testbed) and Step 2 (detector v0,
> offline rules) done; Step 3 (pillar D) in progress — the server runs over
> real UDP; every client datagram carries a session token checked before
> decode, so a forged source address can neither act as a player nor spend
> its rate budget. Sessions are capped per IP and end after 5 s idle, so one
> machine cannot fill the server. Joining takes a cookie round trip, so a
> forged Join cannot point the server's traffic at a bystander. The arena has
> walls, and each client is sent only the players it can see, so a wallhack
> has nothing to draw. The server can run as an origin behind a relay:
> clients only ever hear from the relay's address, and the origin drops
> anything not from the relay under the shared key. No latency margin on
> culling: an enemy appears the tick it comes into sight. The
> relay-to-origin link is sealed (XChaCha20-Poly1305 under subkeys HKDF'd
> from one shared 32-byte secret): an observer of the link learns neither
> client addresses, nor tokens, nor messages. The client-to-relay leg is
> sealed too (protocol v3, the netcode.io model): the game's backend hands
> each client a session id and two keys over HTTPS, the relay re-derives
> the keys from the session id in each datagram — no table, no public-key
> operation — and an on-path attacker who could take a player over a plain
> leg (`sniff` bot: 59 of 59 forgeries accepted) gets nothing through a
> relay (0). The origin binds each player to the session it was admitted
> under. Direct, relay-less runs are still plaintext: lab only. The relay
> drops, statelessly,
> any datagram whose session token was not issued for its source address
> (tokens carry a MAC the relay can check), and gives token-0 datagrams
> (Joins, which the token check must let through) the origin's own per-IP
> budget per tick. The relay also does the Join cookie round trip itself, so
> no Join from a forged source address — one IP or thousands — ever reaches
> the origin. 54% of the standard scenario's client traffic never reaches
> the origin. Version and sessions per IP are still judged at the origin.
> Pillar D's patterns are done; detector v1 (pillar C) judges players
> online, one telemetry record at a time: per session (an id reused after a
> player leaves starts clean), over the lifetime and over the last 100
> shots, so an aimbot switched on for a burst is caught while the burst is
> on. Its false-positive bound is measured on 1008 honest players in full
> arenas — walkers, campers and rushers — with zero flagged.
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
  relay/        pillar D: the only address clients see; forwards to a private origin
  ffi/          C ABI (include/aegis.h): the detector, the client, and the
                evidence over an engine's own world; C examples in examples/
  harness/      runs every bot against the real guards + sim, reports per bot
bindings/
  csharp/       C# over the C ABI (Unity, Godot, Stride, .NET); Aegis.Check
                is run by aegis-ffi's tests
demos/
  godot/        a Godot 4 (.NET) game with Aegis inside (AEGIS_GODOT test)
scenarios/out/  harness telemetry output (gitignored)
```

From another engine (C, C++, C#, anything with a C FFI): `crates/ffi`. An
engine with its own simulation hands Aegis its players and its line of
sight each tick (`aegis_evidence_*`), gets each shot's evidence, and feeds
it to the online detector (`aegis_monitor_*`), whose every line is the
game's to set (`AegisConfig`, `aegis_config_at_tick_rate`). Its clients
speak to the relay through `aegis_client_*`. `examples/engine.c` is the
whole loop in one file; a test builds every example with the platform's C
compiler and checks the header against the library. From C# (Unity, Godot,
Stride, any .NET): `bindings/csharp`, a netstandard2.1 wrapper over the
same library (`AegisMonitor`, `AegisClient`, `AegisEvidence`,
`NoiseHandshake`), held to it by the same kind of test. `demos/godot` is a
Godot 4 game using it on its own world (Godot raycasts answer line of
sight): the aimbot is flagged within seconds, the honest bots never. Its
harder bots found two cheats every detector missed — an aimbot that waits
like a person and a triggerbot under a human hand — which `far_aim`
(accuracy on small, far targets) now catches, at the cost of flagging an
honest pro now and then (`demos/godot/README.md`).

The edge key rotates in epochs (`Relay::add_edge`, `Relay::retire_edge`),
so a leaked key exposes one epoch's traffic. Without a backend (LAN,
community servers), feature `noise` lets the relay hand out sessions itself
over a Noise NK handshake, answered with a Diffie-Hellman only after a
cookie round trip. A C client does the handshake with `aegis_noise_*`
(aegis-ffi built with feature `noise`) and hands the keys it gets to
`aegis_client_*` like a backend's.

Run the lab:

```
cargo run -p aegis-harness              # every bot vs guards + detector
cargo run -p aegis-harness -- --udp     # same, over real UDP on loopback
cargo run -p aegis-harness -- --relay   # same, server hidden behind a relay
cargo run -p aegis-harness -- sweep     # honest population per detector signal
```

Both network runs must write telemetry byte-identical to the in-process run
(the relay run also accounts, label by label, for every datagram it stopped
at the edge):

```
cmp scenarios/out/standard.jsonl scenarios/out/standard.udp.jsonl
cmp scenarios/out/standard.jsonl scenarios/out/standard.relay.jsonl
```

## License

Apache-2.0. See `LICENSE`.
