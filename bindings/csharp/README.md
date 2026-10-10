# Aegis for C#

A thin wrapper over the C ABI (`crates/ffi/include/aegis.h`), targeting
netstandard2.1: Unity 2021.2+, Godot 4 (C#), Stride, and any .NET 5+.

```
cargo build -p aegis-ffi --release                  # aegis_ffi.dll / libaegis_ffi.so / .dylib
cargo build -p aegis-ffi --release --features noise # also NoiseHandshake
```

Put the native library next to the game's executable, or in Unity under
`Assets/Plugins/<platform>/`. Add `Aegis/*.cs` to the game (or reference
`Aegis.csproj`).

| Type | What for |
|------|----------|
| `AegisMonitor` | the online detector: write records, `Poll` alerts |
| `AegisEvidence` | each shot's evidence, over the game's own players and line of sight |
| `AegisClient` | a game client talking to an Aegis relay; the game owns the socket |
| `NoiseHandshake` | keys from a relay with no backend (feature `noise`) |

On the server, each tick:

```csharp
evidence.BeginTick(tick, players, count, (fx, fy, tx, ty) => world.LineOfSight(fx, fy, tx, ty));
var s = evidence.Shot(tick, seen, shooter, alive, aimX, aimY);
if (s.HasGlimpse) monitor.Glimpse(tick, shooter, s);
if (s.HasAim) monitor.Shot(tick, shooter, hit, s);
while (monitor.Poll(out var alert)) Review(alert);   // evidence for a human, not a ban
```

The `seen` tick must be proven: a MAC'd per-snapshot proof the client
echoes, at most 16 ticks old, never going back. Otherwise a client picks the
picture it is judged in.

Notes:
- A handle is for one thread at a time.
- The `sees` callback runs only during `BeginTick`/`Joined`. An exception it
  throws comes back out of that call. Calling back in on the same handle is
  refused (`Status.Busy`). A `Dispose` from inside it frees the handle once
  the call returns.
- The callback is a static method with `[MonoPInvokeCallback]`, as IL2CPP
  requires; nothing else is needed for Unity.
- `bool` never crosses as a struct field (a C# `bool` marshals as 4 bytes);
  `Player.AliveByte` and the `ShotEvidence.*Byte` fields are what C sees.

`Aegis.Check` is the binding run against the real library:
`crates/ffi/tests/csharp.rs` builds it, runs the engine loop of
`examples/engine.c` through it, and compares every struct's size and field
offsets and every constant with Rust's.
