# Security policy

Aegis is a security project, so a hole in it matters more than a bug
elsewhere. Thank you for looking.

## Reporting a vulnerability

**Do not open a public issue.** Report it privately through GitHub:
[Report a vulnerability](https://github.com/NattawatRyu/aegis/security/advisories/new)
(the repository's *Security* tab, *Report a vulnerability*).

Please include:
- what an attacker can do, and what they need first (a client's keys, a
  position on the path, the relay's address, nothing);
- the crate and file, and a way to reproduce it — a failing test, a bot in
  `crates/client-sdk`, or a sequence of datagrams.

What to expect: an acknowledgement within 7 days, and a fix or a decision
within 30. You will be credited in the fix unless you ask not to be.

## Scope

Aegis is **pre-alpha** and has no released versions: only the `main`
branch is supported. In scope:

- forging, replaying or taking over another player's session or inputs;
- getting past a guard the code claims (each guard's `STOPS:` line in
  `crates/server/src/guards/` says what it claims);
- reaching or learning the origin's address from behind a relay;
- reading or altering sealed traffic (client↔relay, relay↔origin, the Noise
  handshake);
- memory unsafety or a panic across the C ABI (`crates/ffi`).

Not vulnerabilities, but welcome as ordinary issues:

- a cheat the detector does not flag, or an honest player it does — the
  detector flags for human review and is expected to be tuned;
- volumetric denial of service. The relay limits what reaches the origin,
  but no software stops a flood bigger than the link.

## Known limits

See *Known limits* in the [README](README.md#known-limits). They are known,
and reports that add evidence to them (especially from real networks) are
welcome.
