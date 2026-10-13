# Contributing to Aegis

Thanks for looking. The most useful contribution right now is evidence from
a real network. Run `aegis-serve` (or your game with Aegis inside) with
`--log records.jsonl`, then open an issue saying what you saw: who was
flagged, who was not, and the network you ran on. Aegis has only been
measured on loopback and on bots.

Security issues go through [SECURITY.md](SECURITY.md), not public issues.

## Build and test

Install [rustup](https://rustup.rs). The toolchain is pinned in
`rust-toolchain.toml`.

```
cargo test --workspace                                        # everything; Noise on (aegis-serve unifies features)
cargo test -p aegis-protocol -p aegis-relay -p aegis-ffi      # Noise off
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Optional checks:

| Check | How to turn it on |
|-------|-------------------|
| C# binding | Install the .NET SDK. Without it the test is skipped; CI requires it. |
| Godot demo | Set `AEGIS_GODOT` to a Godot 4.7.2 .NET console executable. |
| C examples | Install a C compiler (MSVC, gcc or clang). |

The harness needs one loopback IP per bot, so it does not run on macOS.
CI runs on Ubuntu 24.04 and Windows.

## How the code is laid out, and the rules that keep it honest

- **One defense per file** (`crates/server/src/guards/`) and **one attack
  per file** (`crates/client-sdk/src/`). A new guard comes with the bot that
  trips it. The test `every_reject_reason_fires` fails if any rejection is
  never triggered.
- **One detector per file** (`crates/detector/src/detectors/`). The test
  `every_flag_fires` fails if any flag is never raised.
- **Network runs must match the in-process run byte for byte.** The UDP
  and relay transports write telemetry identical to the in-process run. CI
  checks this with `cmp`. A change that makes them differ is a bug in the
  change.
- **A bug fix comes with a test that fails on the old code.** A rewrite is
  compared against the code it replaces before the old code goes.
- **Every queue is bounded.**
- **Thresholds are measured, not guessed.** Use `aegis-harness sweep`,
  `lag`, `net` and `interp` before and after a change to a detector line.
  Say what you measured in the PR.
- **Changes to the ABI** update `aegis.h`, the C# binding,
  `AEGIS_ABI_VERSION` and the CHANGELOG together. Tests check that they
  agree.

## Pull requests

For anything beyond a small fix, open an issue with the design first. Say
what changes, what could break, and how you will show that it did not.
Then send the PR with the commands you ran and what they printed.

## Ethics

Every bot here attacks a server you run yourself. Do not point them at
anything you do not own or are not authorized to test.

By contributing you agree your work is licensed under Apache-2.0.
