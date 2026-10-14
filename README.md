# Aegis

[![ci](https://github.com/NattawatRyu/aegis/actions/workflows/ci.yml/badge.svg)](https://github.com/NattawatRyu/aegis/actions/workflows/ci.yml)

Open-source toolkit that helps game developers build multiplayer games that
resist **cheating** and **DDoS** from day one — by fixing the *architecture*,
not by chasing individual cheats.

> Status: **pre-alpha — usable to try, not to ship.** All four pillars have
> working code, the numbers below come from tests and measurements in this
> repository, and CI runs them on
> Linux, Windows and in a real Godot game. It has **not** run on a real
> network with real players, and this project does not plan to run it there:
> read [Known limits](#known-limits) first.
>
> - **Pillar D (network):** an authoritative server behind a relay that
>   hides its address, sealed traffic, stateless edge filtering, join
>   cookies. Details below.
> - **Pillar C (detection):** an online detector that flags players for
>   human review: aim, reaction, accuracy against range, foresight.
> - **Pillar B (netcode):** client prediction that matches the server
>   bit-exactly, interpolation, and inputs that claim the frame they were
>   chosen on.
> - **Pillar A (lab):** a bot for every cheat class, run against the real
>   guards, in-process, over UDP and through the relay.
> - **Engines:** a C ABI and a C# binding, with a Godot demo.
> - **Run it:** `aegis-serve` is a live server a client can join today.
>
> How pillar D was built, step by step — the server runs over
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

## Getting started

**See it work (2 minutes).** Needs [rustup](https://rustup.rs); the toolchain
is pinned in `rust-toolchain.toml` and installed on first build.

```
git clone https://github.com/NattawatRyu/aegis
cd aegis
cargo run --release -p aegis-netguard-serve -- --bots honest:3,aimbot --ticks 300
```

Three honest bots and an aimbot play for 10 seconds. The aimbot is flagged
within about 3 seconds (`alert ... (aimbot) reason=aim_exact`), the honest
bots never, and each player's tally is printed at the end.

**Put it in your engine (C, C++, C#: Unity, Godot, Stride).** Download
`aegis-<version>-<platform>` from
[Releases](https://github.com/NattawatRyu/aegis/releases). It holds:

| Path | What |
|------|------|
| `lib/` | the native library (`aegis_ffi.dll` + import lib, or `libaegis_ffi.so`), built with Noise |
| `include/aegis.h` | the C header |
| `examples/` | `engine.c` (the server's loop), `client.c`, `monitor.c`, `noise.c` |
| `csharp/` | the C# binding source (`Aegis.csproj`, netstandard2.1); see its README |
| `bin/` | `aegis-serve`, to test your client against |

Built for Windows x64 and Linux x64. The Linux build needs the glibc of
Ubuntu 24.04 or newer. Anywhere else (macOS, older Linux, ARM), build it:
`cargo build --release -p aegis-netguard-ffi --features noise`. macOS is not
tested.

**Use it from Rust.** The crates are on crates.io under the
`aegis-netguard-` prefix. Rename them back on import and the code reads
`use aegis_server`, as in every example here:

```toml
aegis-server = { package = "aegis-netguard-server", version = "0.1.0-alpha.2" }
aegis-detector = { package = "aegis-netguard-detector", version = "0.1.0-alpha.2" }
```

The live server installs with `cargo install aegis-netguard-serve`
(binary: `aegis-serve`).

Before any real player joins, read [Known limits](#known-limits).
Contributing: [CONTRIBUTING.md](CONTRIBUTING.md). What changed:
[CHANGELOG.md](CHANGELOG.md).

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
  serve/        aegis-serve: a live game at 30 Hz behind a Noise relay, the
                detector watching, bots to play against
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
speak to the relay through `aegis_client_*`, and may draw behind the
newest snapshot (`aegis_client_view`, inputs claiming the frame drawn with
`aegis_client_input_drawn`, or `aegis_client_input_at` for an engine
that interpolates on its own) and predict themselves
(`aegis_client_predict`). `examples/engine.c` is the
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

Run a live game a client can join:

```
cargo run --release -p aegis-netguard-serve -- --bots honest:3,aimbot --key-out relay.key
```

It prints the relay's address and public key and writes the key to
`relay.key`. A client that knows only those two runs the Noise handshake,
joins and plays (`NoiseHandshake` + `AegisClient` from C#, `aegis_noise_*` +
`aegis_client_*` from C). Every alert is printed as it is raised; `--log
records.jsonl` keeps the telemetry, `--ticks N` stops and prints each
player's tally. On a laptop the aimbot is flagged within 3 s, the honest
bots never. It listens on loopback by default; `--listen 0.0.0.0:7777`
takes players from the network (read [Known limits](#known-limits) first).

Run the lab:

```
cargo run -p aegis-harness              # every bot vs guards + detector
cargo run -p aegis-harness -- --udp     # same, over real UDP on loopback
cargo run -p aegis-harness -- --relay   # same, server hidden behind a relay
cargo run -p aegis-harness -- sweep     # honest population per detector signal
cargo run -p aegis-harness -- net       # honest play + client prediction under jitter + loss (net [seeds] [rtt] [down‰])
cargo run -p aegis-harness -- interp    # interpolation: picture pace vs down jitter + delay (interp [seeds] [rtt] [down‰])
```

Both network runs must write telemetry byte-identical to the in-process run
(the relay run also accounts, label by label, for every datagram it stopped
at the edge):

```
cmp scenarios/out/standard.jsonl scenarios/out/standard.udp.jsonl
cmp scenarios/out/standard.jsonl scenarios/out/standard.relay.jsonl
```

## Known limits

What is measured, it is measured on loopback and on bots. What has not been
measured is listed here so nobody finds it the hard way. If you run Aegis
on a real network, a `--log` jsonl or an issue about what you saw is the
most useful contribution there is.

- **Never run on a real network, and no plan to.** Jitter and loss have been
  simulated in-process only. Untested: real paths, NATs, Wi-Fi, mobile
  networks. Testing on a real network is left to whoever deploys Aegis.
- **A NAT that changes a client's port mid-game drops it.** The session
  token is bound to the client's address, so the relay drops everything from
  the new port. The client gets no more snapshots and must join again;
  `Connection` and `aegis_client_*` do not rejoin by themselves.
- **At most 4 players behind one IP** (`MAX_PER_IP`). A café, a dorm or a
  carrier-grade NAT can have more.
- **Large snapshots exceed a typical MTU.** A datagram may be up to 2048
  bytes (`MAX_DATAGRAM`). With very many players in sight a snapshot passes
  1472 bytes and relies on IP fragmentation, which some networks drop.
- **Detector lines are set from bots, not people.** Honest bots are never
  flagged (1008 measured), but a strong human is not a bot: in the Godot
  demo a fast honest "pro" bot was flagged by `far_aim` in 2 of 48 runs.
  **Flags are evidence for a human reviewer. Do not ban on them
  automatically.** A game can set every line itself (`Config`).
- **`aegis-serve` runs the relay and the origin on one machine.** The
  origin only listens on loopback, but a flood aimed at the relay's address
  lands on the origin's machine all the same. To shield the origin, the
  relay must run on another host (`aegis_relay::Relay` +
  `NetServer::behind_relay`), and `aegis-serve` cannot split them yet. The
  relay's Noise key is new on every start, so clients must read it again
  after a restart.
- **`--direct` is plaintext**: for debugging on your own machine only.
- **macOS:** only 127.0.0.1 can be bound on loopback, so the harness and
  `aegis-serve`'s bots (one loopback IP each) do not run there.
- **No stable versions.** Every crate is 0.1.0-alpha.2. The wire protocol
  and the C ABI (`AEGIS_ABI_VERSION`, now 4) change when they need to. An
  alpha can break either one.

Security issues: see [SECURITY.md](SECURITY.md). Please do not open a
public issue for them.

## License

Apache-2.0. See `LICENSE`.

If Aegis is useful to you, you can support it on
[Ko-fi](https://ko-fi.com/nattawatchim).
