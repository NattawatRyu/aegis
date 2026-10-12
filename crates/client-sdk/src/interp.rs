//! Interpolation of what the server shows (pillar B, B4).
//!
//! Snapshots do not land one per tick: jitter bunches them, loss leaves
//! holes. A client that draws the newest one stalls on a hole and jumps when
//! two land at once. An interpolating client draws a little in the past
//! instead — `delay` ticks behind the newest snapshot — so that, most of the
//! time, a snapshot on either side of the moment drawn has already landed,
//! and every player can be placed between the two.
//!
//! What it draws is also what its inputs claim ([`View::claim`]): the
//! first kept snapshot at or after the moment drawn, never an older one
//! than it claimed before. The server judges every shot in the picture its
//! input claims (`server::history`), so the claim must be the picture on
//! screen. The set of players and their health and life are the claimed
//! snapshot's; only positions are blended, and only for a player alive in
//! both snapshots around the moment — a respawn is a jump, not a glide
//! across the arena.
//!
//! The input says how long before the claimed tick the moment drawn sat
//! ([`View::offset`], v6), and the server blends its own history back to
//! it. Only ever *before*: claiming the snapshot after the moment, a client
//! never needs to say it drew later than a snapshot it proved — the server
//! refuses that, as it would judge the shot in a picture newer than any
//! the client had. Up to two ticks before (a lost snapshot is bridged
//! within that); further is clamped. Judged in the claimed *whole* tick
//! instead (v5), a blending client's honest aim was up to half a tick off,
//! and the lab's honest players tripped `foresight` — up to 12 of 96 per
//! cell (`aegis-harness interp`, rtt 4, 10% of snapshots lost), against 0
//! drawing whole ticks on the same clock.
//!
//! It never extrapolates: past the newest snapshot it holds. Guessing where
//! an enemy went is guessing, and the server would judge the shot against
//! where it was.
//!
//! The clock that sets the moment drawn runs at the rate the caller says
//! (`dt`, in ticks), stretched or squeezed by up to [`SLEW`] to drift back to
//! `delay` behind the newest, and jumps forward only when it has fallen
//! [`SNAP`] ticks behind. It never runs backwards.

use std::collections::VecDeque;

use aegis_protocol::{PlayerState, Vec2, OFFSET_UNIT};

/// Ticks of snapshots kept, counting back from the newest: as many as the
/// proofs a [`crate::conn::Connection`] keeps, so every claim can be proven.
pub const KEEP: u32 = 16;

/// Most the clock is stretched or squeezed per tick of `dt`, to drift back
/// to `delay` behind the newest snapshot.
pub const SLEW: f64 = 0.1;

/// A clock this many ticks behind where it should be jumps there.
pub const SNAP: f64 = 8.0;

/// Most `delay` may be: less than [`SNAP`], or the clock would jump on
/// its own target.
pub const MAX_DELAY: f64 = 6.0;

/// One frame to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// The moment drawn, in ticks (fractional between snapshots).
    pub at: f64,
    /// The tick an input chosen on this frame claims, and its proof.
    pub claim: u32,
    pub proof: u32,
    /// How long before `claim` the moment `at` sat, in 1/[`OFFSET_UNIT`]
    /// ticks: 0 down to `i8::MIN` (two ticks), clamped. What the input says
    /// (v6), so the server judges the shot at the moment drawn.
    pub offset: i8,
    /// The claimed snapshot's players, each placed at `at`.
    pub players: Vec<PlayerState>,
}

/// How steadily the moment drawn advanced, frame by frame. Kept for any
/// client — the lab scores one that draws the newest snapshot the same way.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Pace {
    /// Frames judged: every one after the first.
    pub frames: u32,
    /// Advanced by the time that passed, within [`SLEW`].
    pub steady: u32,
    /// Advanced by less: a stall, waiting for a snapshot.
    pub held: u32,
    /// Advanced by more: a jump.
    pub jumped: u32,
    /// Drawn between two snapshots with a lost or late one between them.
    pub bridged: u32,
    /// Sum over the frames of how far the moment drawn was behind the
    /// newest snapshot, in ticks.
    pub behind: f64,
}

impl Pace {
    /// A frame drawn `now`, the last one `was` (`None`: this is the first),
    /// after `dt` ticks, with `newest` the newest snapshot landed.
    pub fn frame(&mut self, was: Option<f64>, now: f64, dt: f64, newest: u32) {
        let Some(was) = was else { return };
        const EPS: f64 = 1e-9;
        let moved = now - was;
        self.frames += 1;
        self.behind += f64::from(newest) - now;
        if moved < dt * (1.0 - SLEW) - EPS {
            self.held += 1;
        } else if moved > dt * (1.0 + SLEW) + EPS {
            self.jumped += 1;
        } else {
            self.steady += 1;
        }
    }

    /// Mean ticks the moment drawn was behind the newest snapshot.
    pub fn mean_behind(&self) -> f64 {
        self.behind / f64::from(self.frames.max(1))
    }

    /// Pool another client's pace into this one.
    pub fn add(&mut self, o: &Pace) {
        self.frames += o.frames;
        self.steady += o.steady;
        self.held += o.held;
        self.jumped += o.jumped;
        self.bridged += o.bridged;
        self.behind += o.behind;
    }
}

pub struct Interpolator {
    delay: f64,
    /// (tick, proof, players), ascending by tick, none older than
    /// [`KEEP`] ticks behind the newest.
    snaps: VecDeque<(u32, u32, Vec<PlayerState>)>,
    /// The moment last drawn.
    clock: Option<f64>,
    claim: Option<u32>,
    pace: Pace,
}

impl Interpolator {
    /// Draw `delay` ticks behind the newest snapshot: clamped to
    /// 0..=[`MAX_DELAY`], 0 if not a number.
    pub fn new(delay: f64) -> Self {
        let delay = if delay.is_nan() { 0.0 } else { delay.clamp(0.0, MAX_DELAY) };
        Self { delay, snaps: VecDeque::new(), clock: None, claim: None, pace: Pace::default() }
    }

    /// The snapshot of `tick` landed, with its `proof`. One already kept, or
    /// older than [`KEEP`] ticks behind the newest, is ignored.
    pub fn snapshot(&mut self, tick: u32, proof: u32, players: &[PlayerState]) {
        if self.newest().is_some_and(|n| tick + KEEP <= n) {
            return;
        }
        let Err(i) = self.snaps.binary_search_by_key(&tick, |s| s.0) else { return };
        self.snaps.insert(i, (tick, proof, players.to_vec()));
        let newest = self.newest().unwrap_or(tick);
        while self.snaps.front().is_some_and(|s| s.0 + KEEP <= newest) {
            self.snaps.pop_front();
        }
    }

    /// The newest snapshot landed.
    pub fn newest(&self) -> Option<u32> {
        self.snaps.back().map(|s| s.0)
    }

    /// `dt` ticks have passed (non-finite or negative: none): the frame to
    /// draw now. `None` until a snapshot has landed.
    pub fn advance(&mut self, dt: f64) -> Option<View> {
        let newest = self.newest()?;
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let target = f64::from(newest) - self.delay;
        let was = self.clock;
        let mut at = match was {
            None => target,
            Some(c) => {
                let (free, slew) = (c + dt, SLEW * dt);
                if (target - free).abs() <= slew {
                    target
                } else {
                    free + slew.copysign(target - free)
                }
            }
        };
        if target - at > SNAP {
            at = target;
        }
        // Never past the newest, never back, never before the oldest kept.
        at = at.min(f64::from(newest));
        if let Some(c) = was {
            at = at.max(c);
        }
        at = at.max(f64::from(self.snaps[0].0));
        self.clock = Some(at);

        // The snapshots either side of `at`: `lo` at or before, `hi` after.
        let i = self.snaps.partition_point(|s| f64::from(s.0) <= at);
        let lo = &self.snaps[i - 1];
        let hi = self.snaps.get(i);
        // Claimed: the first kept at or after `at` (`hi` is missing only at
        // the newest, where `at` is `lo`).
        let after = if f64::from(lo.0) == at { lo.0 } else { hi.map_or(lo.0, |h| h.0) };
        let claim = self.claim.map_or(after, |c| c.max(after));
        self.claim = Some(claim);
        let (_, proof, shown) = self.snaps.iter().find(|s| s.0 == claim).unwrap_or(lo);

        let players = shown
            .iter()
            .map(|&p| {
                let Some(h) = hi.filter(|_| p.alive) else { return p };
                let alive_in = |s: &[PlayerState]| s.iter().find(|q| q.id == p.id && q.alive).map(|q| q.pos);
                let (Some(a), Some(b)) = (alive_in(&lo.2), alive_in(&h.2)) else { return p };
                let k = ((at - f64::from(lo.0)) / f64::from(h.0 - lo.0)) as f32;
                PlayerState { pos: Vec2::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k), ..p }
            })
            .collect();

        if hi.is_some_and(|h| h.0 > lo.0 + 1) && at > f64::from(lo.0) {
            self.pace.bridged += 1;
        }
        self.pace.frame(was, at, dt, newest);
        let back = (at - f64::from(claim)) * f64::from(OFFSET_UNIT);
        let offset = back.round().clamp(f64::from(i8::MIN), 0.0) as i8;
        Some(View { at, claim, proof: *proof, offset, players })
    }

    /// How steadily it has drawn so far.
    pub fn pace(&self) -> Pace {
        self.pace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: u8, x: f32, alive: bool) -> PlayerState {
        PlayerState { id, pos: Vec2::new(x, 0.0), health: 100, alive }
    }

    /// Snapshot `t`: player 1 at x = t, player 2 at x = -t.
    fn snap(i: &mut Interpolator, t: u32) {
        i.snapshot(t, t * 10, &[p(1, t as f32, true), p(2, -(t as f32), true)]);
    }

    fn x(v: &View, id: u8) -> f32 {
        v.players.iter().find(|q| q.id == id).unwrap().pos.x
    }

    #[test]
    fn nothing_to_draw_before_a_snapshot() {
        let mut i = Interpolator::new(2.0);
        assert_eq!(i.advance(1.0), None);
    }

    /// With every snapshot landing on time, it draws exactly `delay` behind,
    /// one tick per tick, and claims what it draws.
    #[test]
    fn steady_snapshots_draw_delay_behind() {
        let mut i = Interpolator::new(2.0);
        for t in 1..=3 {
            snap(&mut i, t);
        }
        let v = i.advance(1.0).unwrap();
        assert_eq!((v.at, v.claim, v.proof, v.offset, x(&v, 1)), (1.0, 1, 10, 0, 1.0));
        for t in 4..=10 {
            snap(&mut i, t);
            let v = i.advance(1.0).unwrap();
            assert_eq!((v.at, v.claim, v.proof), (f64::from(t) - 2.0, t - 2, (t - 2) * 10));
        }
        let s = i.pace();
        assert_eq!((s.frames, s.steady, s.held, s.jumped, s.bridged), (7, 7, 0, 0, 0));
        assert_eq!(s.mean_behind(), 2.0);
    }

    /// A lost snapshot is bridged: the players glide across it, and the claim
    /// is the snapshot after the moment drawn, with how long before it.
    #[test]
    fn a_lost_snapshot_is_bridged() {
        let mut i = Interpolator::new(1.0);
        snap(&mut i, 4);
        snap(&mut i, 6); // 5 lost
        i.clock = Some(4.0);
        i.claim = Some(4);
        let v = i.advance(0.5).unwrap(); // target 5: 4.5 then +0.05
        assert!((v.at - 4.55).abs() < 1e-9, "{}", v.at);
        assert_eq!((v.claim, v.offset), (6, -93), "drawn 1.45 ticks before 6");
        assert!((x(&v, 1) - 4.55).abs() < 1e-5 && (x(&v, 2) + 4.55).abs() < 1e-5);
        let v = i.advance(0.5).unwrap();
        assert!((v.at - 5.0).abs() < 1e-9);
        assert_eq!((v.claim, v.offset), (6, -OFFSET_UNIT), "a tick before it");
        assert_eq!(i.pace().bridged, 2);
    }

    /// Never a positive offset; past two ticks before the claim it is
    /// clamped, and on a kept tick it is 0.
    #[test]
    fn the_offset_is_never_after_the_claim() {
        let mut i = Interpolator::new(0.0);
        snap(&mut i, 4);
        snap(&mut i, 8);
        i.clock = Some(4.0);
        i.claim = Some(4);
        let v = i.advance(0.5).unwrap(); // 4.55
        assert_eq!((v.claim, v.offset), (8, i8::MIN), "3.45 ticks before 8: clamped to two");
        let mut i = Interpolator::new(0.0);
        snap(&mut i, 4);
        assert_eq!(i.advance(1.0).unwrap().offset, 0, "on 4 itself");
    }

    /// Past the newest it holds: nothing is extrapolated, and the stall is
    /// counted as held.
    #[test]
    fn it_never_draws_past_the_newest() {
        let mut i = Interpolator::new(0.0);
        snap(&mut i, 5);
        assert_eq!(i.advance(1.0).unwrap().at, 5.0);
        let v = i.advance(1.0).unwrap();
        assert_eq!((v.at, v.claim, x(&v, 1)), (5.0, 5, 5.0));
        assert_eq!((i.pace().frames, i.pace().held), (1, 1));
    }

    /// Behind its delay it runs at most SLEW fast, ahead at most SLEW slow;
    /// SNAP behind it jumps.
    #[test]
    fn the_clock_drifts_back_to_its_delay_and_snaps_when_far_behind() {
        let mut i = Interpolator::new(2.0);
        snap(&mut i, 10);
        assert_eq!(i.advance(1.0).unwrap().at, 10.0, "one snapshot: nothing older to draw");
        snap(&mut i, 11);
        let v = i.advance(1.0).unwrap();
        assert!((v.at - 10.9).abs() < 1e-9, "squeezed: {}", v.at);
        assert_eq!(i.pace().steady, 1, "a squeezed tick is still steady");
        let mut i = Interpolator::new(2.0);
        snap(&mut i, 10);
        snap(&mut i, 12);
        assert_eq!(i.advance(1.0).unwrap().at, 10.0);
        snap(&mut i, 13);
        assert_eq!(i.advance(1.0).unwrap().at, 11.0, "within SLEW of its delay: on it");
        snap(&mut i, 15);
        let v = i.advance(1.0).unwrap();
        assert!((v.at - 12.1).abs() < 1e-9, "stretched toward 13: {}", v.at);
        snap(&mut i, 25);
        let v = i.advance(1.0).unwrap();
        assert_eq!(v.at, 23.0, "more than SNAP behind: jumps to the target");
        assert_eq!((v.claim, v.offset), (25, i8::MIN), "the first kept after 23");
        assert_eq!((i.pace().steady, i.pace().jumped), (2, 1));
    }

    /// The claim never goes back, even when a late snapshot lands under it:
    /// the moment drawn is still said, as a longer offset.
    #[test]
    fn the_claim_never_goes_back() {
        let mut i = Interpolator::new(0.0);
        snap(&mut i, 4);
        snap(&mut i, 8);
        i.clock = Some(5.9);
        i.claim = Some(4);
        let v = i.advance(0.1).unwrap(); // 6.01
        assert_eq!((v.claim, v.offset), (8, -127));
        snap(&mut i, 6);
        snap(&mut i, 7);
        let v = i.advance(0.1).unwrap(); // 6.12
        assert_eq!((v.claim, v.offset), (8, -120), "7 is after it, but 8 was claimed");
        assert!((x(&v, 1) - 6.12).abs() < 1e-5 && (x(&v, 2) + 6.12).abs() < 1e-5, "blended 6 to 7");
    }

    /// Who is shown, their health and their life are the claimed snapshot's;
    /// a player dead on either side is not blended — a respawn is a jump.
    #[test]
    fn only_the_living_glide() {
        let mut i = Interpolator::new(0.0);
        i.snapshot(1, 0, &[p(1, 0.0, true), p(2, 0.0, false), p(3, 0.0, true)]);
        i.snapshot(2, 0, &[p(1, 10.0, true), p(2, 10.0, true), p(4, 9.0, true)]);
        i.clock = Some(1.0);
        i.claim = Some(1);
        let v = i.advance(0.25).unwrap(); // 1.25, stretched to 1.275
        assert_eq!((v.claim, v.offset), (2, -46));
        let ids: Vec<u8> = v.players.iter().map(|q| q.id).collect();
        assert_eq!(ids, [1, 2, 4], "3 is not in the claimed snapshot");
        assert!((x(&v, 1) - 2.75).abs() < 1e-5, "alive both sides: blended");
        assert_eq!((x(&v, 2), v.players[1].alive), (10.0, true), "dead the tick before: a jump, not a glide");
        assert_eq!(x(&v, 4), 9.0, "new in the claim: as it is");
    }

    /// Kept: KEEP ticks back from the newest, so every claim has a proof.
    /// Older, or already kept, is ignored.
    #[test]
    fn keeps_what_can_be_claimed() {
        let mut i = Interpolator::new(0.0);
        for t in 1..=40 {
            snap(&mut i, t);
        }
        assert_eq!((i.snaps.len(), i.snaps[0].0), (KEEP as usize, 40 - KEEP + 1));
        snap(&mut i, 40 - KEEP);
        i.snapshot(30, 999, &[]);
        assert_eq!(i.snaps.len(), KEEP as usize);
        assert_eq!(i.snaps.iter().find(|s| s.0 == 30).unwrap().1, 300, "the first copy stays");
    }

    #[test]
    fn delay_and_time_are_sanitised() {
        assert_eq!(Interpolator::new(f64::NAN).delay, 0.0);
        assert_eq!(Interpolator::new(-1.0).delay, 0.0);
        assert_eq!(Interpolator::new(100.0).delay, MAX_DELAY);
        let mut i = Interpolator::new(0.0);
        snap(&mut i, 3);
        i.advance(1.0);
        snap(&mut i, 4);
        assert_eq!(i.advance(f64::NAN).unwrap().at, 3.0, "no time passed");
        assert_eq!(i.advance(-5.0).unwrap().at, 3.0);
    }
}
