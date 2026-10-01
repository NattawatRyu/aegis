//! Guard registry — every input-trust defense the server runs, in one place.
//!
//! Each guard lives in its own file and owns exactly one concern. To add a new
//! defense: create `guards/<name>.rs`, implement [`InputGuard`] (or a stage
//! function, for guards that don't run per-input), and add one line to
//! [`Pipeline::standard`]. Nothing else in the server needs to change.
//!
//! Six guards run at their own stage rather than in the per-input pipeline,
//! because they act on data the pipeline never sees:
//!   - [`session`] — runs first, on the source address + token header.
//!   - [`source_rate`] — then, per player or per source IP, before any decode.
//!   - [`packet`]  — runs at decode, on the raw datagram bytes.
//!   - [`version`] — runs at `Join`, on the client's declared protocol version.
//!   - [`ip_sessions`] — runs at admission, on the joining IP's live sessions.
//!   - [`joined`]  — runs before the pipeline, on the admitted source addresses.
//!
//! Hit / movement authority is NOT a guard: the protocol gives a client no way
//! to assert a position or a hit, so there is nothing to reject. That defense
//! is structural and lives in [`crate::sim`].

use aegis_protocol::{PlayerId, Vec2};

pub mod ip_sessions;
pub mod session;
pub mod source_rate;
pub mod version;
pub mod packet;
pub mod joined;
pub mod sanity;
pub mod input_rate;
pub mod replay;
pub mod move_speed;

/// Per-input context handed to every guard.
#[derive(Debug, Clone, Copy)]
pub struct GuardCtx {
    /// The server tick this input is being folded into.
    pub tick: u32,
    pub player: PlayerId,
}

/// A decoded, not-yet-trusted input. Guards may mutate it (e.g. clamp
/// `move_dir`) before the sim consumes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientInput {
    pub seq: u32,
    pub tick: u32,
    pub move_dir: Vec2,
    pub aim: Vec2,
    pub shoot: bool,
}

/// Why an input (or join, or packet) was rejected. Every reason is also an
/// anomaly signal the telemetry crate feeds to the detector (pillar C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    IpSessions,
    BadToken,
    SourceRate,
    BadVersion,
    MalformedPacket,
    NotJoined,
    MalformedInput,
    RateExceeded,
    Replay,
}

impl RejectReason {
    /// Every reason, for coverage checks ("does some bot trip each guard?").
    /// A new variant goes here too.
    pub const ALL: [RejectReason; 9] = [
        RejectReason::IpSessions,
        RejectReason::BadToken,
        RejectReason::SourceRate,
        RejectReason::BadVersion,
        RejectReason::MalformedPacket,
        RejectReason::NotJoined,
        RejectReason::MalformedInput,
        RejectReason::RateExceeded,
        RejectReason::Replay,
    ];

    /// Stable string label for telemetry / detector features. Kept in sync with
    /// the enum here so the telemetry crate stays decoupled from server types.
    pub fn label(self) -> &'static str {
        match self {
            RejectReason::IpSessions => "ip_sessions",
            RejectReason::BadToken => "bad_token",
            RejectReason::SourceRate => "source_rate",
            RejectReason::BadVersion => "bad_version",
            RejectReason::MalformedPacket => "malformed_packet",
            RejectReason::NotJoined => "not_joined",
            RejectReason::MalformedInput => "malformed_input",
            RejectReason::RateExceeded => "rate_exceeded",
            RejectReason::Replay => "replay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    /// Passed. `anomaly` = suspicious but not blocked (e.g. a move vector that
    /// had to be clamped). Logged for the detector; the input still applies.
    Ok { anomaly: bool },
    /// Dropped. The input does not reach the sim; counted as an anomaly.
    Rejected(RejectReason),
}

/// One input-trust defense. Implementors hold their own per-player state (a
/// guard that needs history keeps its own map keyed by `PlayerId`), so guards
/// stay independent and separately testable.
pub trait InputGuard: Send {
    fn name(&self) -> &'static str;
    fn check(&mut self, ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict;

    /// Drop everything held about `player`: its session ended and the id may
    /// go to someone else, who must not inherit (say) its last seq. A guard
    /// with per-player state must implement this.
    fn forget(&mut self, _player: PlayerId) {}
}

/// The ordered per-input guard chain. First `Rejected` stops the chain; an
/// `anomaly` from any guard is OR-ed into the final verdict.
pub struct Pipeline {
    guards: Vec<Box<dyn InputGuard>>,
}

impl Pipeline {
    /// The standard chain, in run order. This list IS the documentation of what
    /// the server defends against per input.
    pub fn standard() -> Self {
        Self {
            guards: vec![
                Box::new(sanity::SanityGuard),
                Box::new(input_rate::InputRateGuard::new()),
                Box::new(replay::ReplayGuard::new()),
                Box::new(move_speed::MoveSpeedGuard),
            ],
        }
    }

    /// Names in run order — handy for tests and for a `/defenses` endpoint.
    pub fn names(&self) -> Vec<&'static str> {
        self.guards.iter().map(|g| g.name()).collect()
    }

    /// Every guard forgets `player` (see [`InputGuard::forget`]).
    pub fn forget(&mut self, player: PlayerId) {
        for g in self.guards.iter_mut() {
            g.forget(player);
        }
    }

    pub fn run(&mut self, ctx: &GuardCtx, input: &mut ClientInput) -> GuardVerdict {
        let mut anomaly = false;
        for g in self.guards.iter_mut() {
            match g.check(ctx, input) {
                GuardVerdict::Rejected(r) => return GuardVerdict::Rejected(r),
                GuardVerdict::Ok { anomaly: a } => anomaly |= a,
            }
        }
        GuardVerdict::Ok { anomaly }
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::standard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ClientInput {
        ClientInput {
            seq: 1,
            tick: 1,
            move_dir: Vec2::new(0.3, 0.4),
            aim: Vec2::new(1.0, 0.0),
            shoot: false,
        }
    }

    #[test]
    fn standard_pipeline_runs_all_four_in_order() {
        let p = Pipeline::standard();
        assert_eq!(p.names(), vec!["sanity", "input_rate", "replay", "move_speed"]);
    }

    #[test]
    fn honest_input_passes_clean() {
        let mut p = Pipeline::standard();
        let mut i = input();
        let v = p.run(&GuardCtx { tick: 1, player: 1 }, &mut i);
        assert_eq!(v, GuardVerdict::Ok { anomaly: false });
    }

    /// A reused id starts clean: after `forget`, a fresh client's first input
    /// (seq 1, same tick as the old player's last) is not a "replay" and not
    /// "over rate".
    #[test]
    fn forgotten_player_starts_clean() {
        let mut p = Pipeline::standard();
        let ctx = GuardCtx { tick: 5, player: 1 };
        let mut old = ClientInput { seq: 500, ..input() };
        assert_eq!(p.run(&ctx, &mut old), GuardVerdict::Ok { anomaly: false });
        p.forget(1);
        let mut fresh = input(); // seq 1
        assert_eq!(p.run(&ctx, &mut fresh), GuardVerdict::Ok { anomaly: false });
    }

    #[test]
    fn second_input_same_tick_is_rejected_rate() {
        let mut p = Pipeline::standard();
        let ctx = GuardCtx { tick: 1, player: 1 };
        let mut a = input();
        assert_eq!(p.run(&ctx, &mut a), GuardVerdict::Ok { anomaly: false });
        let mut b = ClientInput { seq: 2, ..input() };
        assert_eq!(p.run(&ctx, &mut b), GuardVerdict::Rejected(RejectReason::RateExceeded));
    }
}
