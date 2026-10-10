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

### Harder bots (`--roster honest,honest,pro,pro,aimbot,humanized,trigger`)

| Bot | What it is | Before far_aim (9 seeds) | With far_aim (24 seeds) |
|-----|------------|--------------------------|-------------------------|
| `pro` | honest; 130–270 ms, 67–133 ms when it heard the enemy coming, 0.03 rad off | flagged in 5 of 9 on the old reaction line, 0 after the fix | **flagged by far_aim in 2 of 48 pro runs** |
| `aimbot` | instant, exact | caught 9/9 (aim_exact by tick ~900) | caught 24/24 |
| `humanized` | waits 200–400 ms, 0.012 rad of noise | missed 9/9, hitting 99–100% | **caught 24/24** |
| `trigger` | human aim, machine trigger | missed 9/9, hitting 100% | **caught 19/24** |

Honest bots: never flagged, by any detector, in any run.

- **The pro was a real false positive.** Every flag was early, on 8–21
  timed shots, from a raw-share line checked after every shot. Reaction now
  flags on the Wilson lower bound of the share at z = 2.5. The lab's output
  is byte-identical before and after; pros are flagged 0 times in 18.
  `tests/godot.rs` replays seeds 2 and 4 and asserts it.
- **What is missed, and why:** neither smart cheat is ever exact (>0.001
  rad) or fast (<100 ms). What gives them away is that they almost never
  miss:

  | | hit rate | median aim error |
  |---|---|---|
  | honest | ~70% | 0.035–0.05 rad |
  | pro | ~88% | 0.018–0.024 rad |
  | humanized | 99–100% | 0.008–0.010 rad |
  | trigger | 100% | 0.017–0.030 rad |

  Raw accuracy was dropped from the suite because honest point-blank
  rushers reach 0.985 in the lab.
- **far_aim** (2026-10-10) is accuracy normalised by range. It looks only
  at shots on targets smaller than 0.04 rad (here, beyond ~300 px) and
  asks how often the aim went through the target in the picture the
  shooter had (`aim_err <= size`, where `size = asin(radius / distance)`
  comes with the evidence). Over a match, far shots aimed inside:

  | | share |
  |---|---|
  | honest | 24–63% |
  | pro | 54–78% |
  | humanized | 85–98% |
  | trigger | 83–100% |

  The pro is close to the cheats, and a pro on a streak (12 of 12, 25 of
  27) crosses the line. Raising the line hardly helped (1 of 96 at 0.75)
  and cost most of the triggerbot catches, so the default is 0.65 — see
  the table in `crates/detector/src/detectors/far_aim.rs`. A tighter
  core (aim within half the target) separated worse: the triggerbot aims
  anywhere inside 0.8 of the hitbox. `--far-rad` and `--far-line` set the
  game's own lines for a sweep.
