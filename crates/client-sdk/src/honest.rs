//! HonestBot — the baseline. Walks forward at a legal unit speed, aims fairly
//! at the nearest visible enemy, increments its seq every tick. Passes every
//! guard clean; the other bots are measured against it.

use super::{my_pos, nearest_enemy, unit_towards, Bot, BotCtx};
use aegis_protocol::{ClientMsg, Vec2};

pub struct HonestBot {
    seq: u32,
    walk: Vec2,
}

impl HonestBot {
    pub fn new() -> Self {
        Self { seq: 0, walk: Vec2::new(1.0, 0.0) }
    }
}

impl Default for HonestBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for HonestBot {
    fn name(&self) -> &'static str {
        "honest"
    }

    fn act(&mut self, ctx: &BotCtx) -> Vec<ClientMsg> {
        self.seq += 1;
        let (aim, shoot) = match nearest_enemy(ctx) {
            Some(e) => (unit_towards(my_pos(ctx).unwrap_or(Vec2::ZERO), e.pos), true),
            None => (self.walk, false),
        };
        vec![ClientMsg::Input { seq: self.seq, tick: ctx.tick, move_dir: self.walk, aim, shoot }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_protocol::PlayerState;

    fn state(id: u8, pos: Vec2) -> PlayerState {
        PlayerState { id, pos, health: 100, alive: true }
    }

    #[test]
    fn no_enemy_walks_without_shooting() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO)];
        let out = b.act(&BotCtx { tick: 1, my_id: 1, snapshot: &snap });
        assert_eq!(out.len(), 1);
        if let ClientMsg::Input { shoot, move_dir, .. } = out[0] {
            assert!(!shoot);
            assert!(move_dir.len() <= 1.0 + 1e-6);
        } else {
            panic!("expected Input");
        }
    }

    #[test]
    fn aims_at_the_enemy_and_shoots() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO), state(2, Vec2::new(3.0, 4.0))];
        let out = b.act(&BotCtx { tick: 1, my_id: 1, snapshot: &snap });
        if let ClientMsg::Input { shoot, aim, .. } = out[0] {
            assert!(shoot);
            // direction to (3,4) normalized is (0.6, 0.8)
            assert!((aim.x - 0.6).abs() < 1e-6 && (aim.y - 0.8).abs() < 1e-6);
        } else {
            panic!("expected Input");
        }
    }

    #[test]
    fn seq_increases_each_tick() {
        let mut b = HonestBot::new();
        let snap = [state(1, Vec2::ZERO)];
        let s1 = seq_of(&b.act(&BotCtx { tick: 1, my_id: 1, snapshot: &snap })[0]);
        let s2 = seq_of(&b.act(&BotCtx { tick: 2, my_id: 1, snapshot: &snap })[0]);
        assert!(s2 > s1);
    }

    fn seq_of(m: &ClientMsg) -> u32 {
        match m {
            ClientMsg::Input { seq, .. } => *seq,
            _ => panic!("expected Input"),
        }
    }
}
