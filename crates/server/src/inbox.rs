//! Each player's accepted inputs between arriving and being folded into the
//! world (pillar B, B1) — a jitter buffer.
//!
//! Before B1 an input was folded in on the tick it arrived, and a second one
//! arriving on the same tick was refused. On a perfect link that is one
//! input a tick; on a real one, a tick of jitter lands two inputs on one
//! tick and none on the next, and a quarter of an honest player's input was
//! refused and logged as a violation (`aegis-harness net`, jitter 1).
//!
//! Here inputs wait, ordered by `seq` — not by arrival, so one overtaken on
//! the way is still folded in its turn — and [`Inbox::pop_all`] takes each
//! player's lowest one per tick. The sim still folds at most one input per
//! player per tick, so a buffer buys a client no extra movement; the queue's
//! depth is capped by [`crate::guards::input_rate`]. On a lockstep link each
//! input is popped on the tick it arrives, as before.

use std::collections::BTreeMap;

use aegis_protocol::PlayerId;

use crate::ClientInput;

#[derive(Default)]
pub struct Inbox {
    lanes: BTreeMap<PlayerId, Lane>,
    /// Counts every push, so pops can follow arrival order across players.
    stamp: u64,
}

#[derive(Default)]
struct Lane {
    /// seq -> (input, arrival stamp, tick it arrived on)
    queued: BTreeMap<u32, (ClientInput, u64, u32)>,
    /// seq and claimed tick of the last input taken from this lane.
    taken: Option<(u32, u32)>,
}

impl Inbox {
    /// Inputs waiting for `player`.
    pub fn len(&self, player: PlayerId) -> usize {
        self.lanes.get(&player).map_or(0, |l| l.queued.len())
    }

    /// Has `seq` been seen already — taken, or older than the last taken, or
    /// waiting? Such an input changes nothing: a copy, or one overtaken by
    /// the inputs after it.
    pub fn is_late(&self, player: PlayerId, seq: u32) -> bool {
        self.lanes.get(&player).is_some_and(|l| l.taken.is_some_and(|(s, _)| seq <= s) || l.queued.contains_key(&seq))
    }

    /// The snapshot ticks claimed by `player`'s nearest known inputs either
    /// side of `seq`: below, the highest waiting seq under it (or the last
    /// one taken); above, the lowest waiting seq over it. An input's claim
    /// must sit between them ([`crate::guards::stale_tick`]).
    pub fn neighbours(&self, player: PlayerId, seq: u32) -> (Option<u32>, Option<u32>) {
        let Some(l) = self.lanes.get(&player) else { return (None, None) };
        let below = l.queued.range(..seq).next_back().map(|(_, (i, ..))| i.tick).or(l.taken.map(|(_, t)| t));
        let above = l.queued.range(seq.saturating_add(1)..).next().map(|(_, (i, ..))| i.tick);
        (below, above)
    }

    /// Queue an input that passed every guard, arrived on tick `now`.
    pub fn push(&mut self, player: PlayerId, input: ClientInput, now: u32) {
        self.stamp += 1;
        self.lanes.entry(player).or_default().queued.insert(input.seq, (input, self.stamp, now));
    }

    /// On tick `now`, take each player's lowest-seq input, ordered by when
    /// each arrived — on a lockstep link, the order they were received in
    /// this tick — with the ticks it waited.
    pub fn pop_all(&mut self, now: u32) -> Vec<(PlayerId, ClientInput, u32)> {
        let mut out: Vec<(u64, PlayerId, ClientInput, u32)> = Vec::new();
        for (&p, l) in &mut self.lanes {
            if let Some((_, (input, stamp, arrived))) = l.queued.pop_first() {
                l.taken = Some((input.seq, input.tick));
                out.push((stamp, p, input, now.saturating_sub(arrived)));
            }
        }
        out.sort_unstable_by_key(|&(stamp, ..)| stamp);
        out.into_iter().map(|(_, p, i, w)| (p, i, w)).collect()
    }

    /// `player`'s session ended: its waiting inputs go with it, and whoever
    /// gets the id next starts with an empty lane.
    pub fn forget(&mut self, player: PlayerId) {
        self.lanes.remove(&player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::Vec2;

    fn input(seq: u32, tick: u32) -> ClientInput {
        ClientInput { seq, tick, move_dir: Vec2::new(1.0, 0.0), aim: Vec2::new(1.0, 0.0), shoot: false }
    }

    fn seqs(v: &[(PlayerId, ClientInput, u32)]) -> Vec<(PlayerId, u32)> {
        v.iter().map(|&(p, i, _)| (p, i.seq)).collect()
    }

    #[test]
    fn one_input_per_player_per_pop_lowest_seq_first() {
        let mut b = Inbox::default();
        for (seq, tick) in [(3, 12), (1, 10), (2, 11)] {
            b.push(1, input(seq, tick), 0);
        }
        assert_eq!(seqs(&b.pop_all(1)), vec![(1, 1)]);
        assert_eq!(seqs(&b.pop_all(1)), vec![(1, 2)]);
        assert_eq!(seqs(&b.pop_all(1)), vec![(1, 3)]);
        assert!(b.pop_all(1).is_empty());
    }

    /// Across players, the order inputs arrived in — not player id order.
    #[test]
    fn players_pop_in_arrival_order() {
        let mut b = Inbox::default();
        b.push(7, input(1, 1), 0);
        b.push(2, input(1, 1), 0);
        b.push(5, input(1, 1), 0);
        assert_eq!(seqs(&b.pop_all(1)), vec![(7, 1), (2, 1), (5, 1)]);
    }

    #[test]
    fn late_means_taken_older_or_waiting() {
        let mut b = Inbox::default();
        assert!(!b.is_late(1, 1));
        b.push(1, input(5, 50), 0);
        assert!(b.is_late(1, 5), "waiting");
        assert!(!b.is_late(1, 4), "nothing taken yet: 4 can still go first");
        b.pop_all(1);
        assert!(b.is_late(1, 5) && b.is_late(1, 4), "taken, and older than taken");
        assert!(!b.is_late(1, 6));
        assert!(!b.is_late(2, 5), "players are independent");
    }

    #[test]
    fn neighbours_are_the_nearest_known_claims() {
        let mut b = Inbox::default();
        assert_eq!(b.neighbours(1, 5), (None, None));
        b.push(1, input(2, 20), 0);
        b.pop_all(1);
        assert_eq!(b.neighbours(1, 5), (Some(20), None), "the last taken");
        b.push(1, input(4, 24), 0);
        b.push(1, input(7, 27), 0);
        assert_eq!(b.neighbours(1, 5), (Some(24), Some(27)));
        assert_eq!(b.neighbours(1, 3), (Some(20), Some(24)));
        assert_eq!(b.neighbours(1, 9), (Some(27), None));
    }

    #[test]
    fn forget_empties_the_lane() {
        let mut b = Inbox::default();
        b.push(1, input(9, 90), 0);
        b.pop_all(1);
        b.push(1, input(10, 91), 0);
        b.forget(1);
        assert_eq!((b.len(1), b.is_late(1, 1), b.neighbours(1, 1)), (0, false, (None, None)));
        assert!(b.pop_all(1).is_empty());
    }
}
