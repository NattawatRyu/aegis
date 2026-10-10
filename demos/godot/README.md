# Aegis in Godot

A Godot 4 (.NET) game using Aegis the way a game with its own netcode would.
The game keeps its own world, its own physics and its own hit resolution;
Aegis only measures.

Each physics tick:
1. `AegisEvidence.BeginTick` gets the players. Line of sight is answered by
   a Godot raycast against the walls.
2. `Shot` gives each shot's evidence.
3. An `AegisMonitor` is fed that evidence, and its alerts are drawn on
   screen. They are evidence for a reviewer, not a ban.

Free-for-all in a walled arena:
- **You** (blue): WASD, mouse, left click.
- **Honest bots** (green): react in 270–530 ms, a little off.
- **The aimbot** (red): fires the tick an enemy comes into sight, dead on.

A flagged player gets a red ring.

```
cargo build -p aegis-ffi --release
mkdir demos/godot/native && cp target/release/aegis_ffi.dll demos/godot/native/   # .so / .dylib elsewhere
# open demos/godot in the Godot .NET editor, Build, Play
```

Headless, as a test:

```
godot --headless --fixed-fps 30 --path demos/godot -- --ticks 9000 --seed 7
```

This prints every alert and each player's stats. It exits 0 when the
aimbot was flagged and no honest bot was. `crates/ffi/tests/godot.rs` runs
3 seeds of it when `AEGIS_GODOT` points at the editor's console executable.

What it showed that the lab could not, over 12 seeds of 5 minutes each:
- **The aimbot** was flagged every time, within 7–22 s.
- **The honest bots** raised no alert in 60 bot-runs. Still, 0–12% of their
  timed shots came out "fast", although none of them reacts in under
  8 ticks. The evidence times a reaction against the enemy nearest the aim,
  which is not always the one the bot meant. Real games will have this
  noise; the reaction line (50%) is far above it here.
- **An aimbot that waits like a person** (demo edited to give it the honest
  delay) is no longer caught by reaction. It is still caught by `aim_exact`.
  The detectors cover for each other.
