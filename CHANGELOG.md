# Changelog

Versions follow [Semantic Versioning](https://semver.org). Before 1.0, an
alpha can break the wire protocol or the C ABI; each entry says which ABI it
ships. A test (`crates/ffi/tests/docs.rs`) fails if the entry for the
current crate version is missing or names another ABI.

## [0.1.0-alpha.1] - 2026-10-13

The first release: usable to try, not to ship. Read the README's
*Known limits* first. Nothing here has run on a real network with real
players. Detector flags are evidence for a human reviewer, not grounds for
an automatic ban.

Ships **ABI 4** and wire protocol v6.

### Pillar D: network
- Authoritative server over UDP. Every datagram carries a session token,
  checked before decoding. Sessions are capped per IP and end after 5 s
  idle.
- Join cookie round trip: a forged Join cannot aim the server at a
  bystander (measured amplification 0.22x).
- Per-player line-of-sight culling with no latency margin, so a wallhack has
  nothing to draw.
- `aegis-relay` hides the origin's address. The relay–origin link is sealed
  (XChaCha20-Poly1305). The client–relay leg is sealed under keys issued by
  the game's backend, or by the relay itself over Noise NK (feature
  `noise`). The relay filters statelessly at the edge, and 54% of the
  standard scenario's traffic never reaches the origin.

### Pillar C: detection
- An online `Monitor` judges each session over its lifetime and over its
  last 100 shots: exact aim, reaction time (Wilson lower bound), accuracy
  on far targets (`far_aim`), and foresight (aim at what the claimed frame
  did not show).
- No honest bot was flagged among 1008 measured. Every line can be set by
  the game (`Config`).

### Pillar B: netcode
- Inputs carry redundant copies and acks. Client prediction is bit-exact
  with the server's movement.
- Interpolation, where every input claims the frame it was chosen on, with
  a backward-only sub-tick offset.
- `input_at`: an engine that interpolates on its own passes only the moment
  it drew. The SDK works out the claim and the offset from the snapshots it
  received.

### Engines
- C ABI (`crates/ffi`, `include/aegis.h`) for the detector, shot evidence
  over the engine's own world, and the client. The C examples are built and
  run in CI.
- C# binding (`bindings/csharp`, netstandard2.1), checked against the C
  layouts in CI.
- A Godot 4 .NET demo, run headless in CI.

### Running it
- `aegis-serve`: a live 30 Hz game behind a Noise relay, with the detector
  watching and bots to play against.
- `aegis-harness`: a bot for every cheat class, run against the real guards
  in-process, over UDP and through the relay, with byte-identical telemetry
  across all three.

### Fixed since the last commit before this release
- A snapshot arriving 16 or more ticks late no longer evicts the proof of
  the newer snapshot sharing its slot. Inputs claiming that newer tick were
  refused as `UnknownTick`.

[0.1.0-alpha.1]: https://github.com/NattawatRyu/aegis/releases/tag/v0.1.0-alpha.1
