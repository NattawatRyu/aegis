//! Aegis authoritative game server.
//!
//! Two layers of defense, kept strictly separate:
//!   - [`sim`] — the world's truth. The server, not the client, owns every
//!     position and every hit (structural anti-teleport/instant-hit).
//!   - [`guards`] — the input-trust pipeline. Each cheat class is one module;
//!     see [`guards`] for the full list and how to add more.
//!
//! [`server`] ties them into a tick (receive -> guards -> sim -> telemetry)
//! without owning a socket; [`net`] puts it behind a UDP socket. The
//! in-process harness and the UDP loop drive the exact same code.

pub mod guards;
pub mod net;
pub mod server;
pub mod sim;

pub use guards::{ClientInput, GuardCtx, GuardVerdict, Pipeline, RejectReason};
pub use net::NetServer;
pub use server::{NetStats, Server, TickOutcome};
pub use sim::{ShotResult, Sim};

#[cfg(test)]
mod integration {
    use super::*;
    use aegis_protocol::Vec2;

    /// The headline property: a speedhacker and an honest player who both push
    /// "full forward" end up at the exact same place. The move_speed guard
    /// clamps the cheat's oversized vector, so the sim moves both by MOVE_SPEED.
    #[test]
    fn speedhacker_and_honest_player_move_the_same_distance() {
        let mut pipe = Pipeline::standard();
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO); // honest
        sim.spawn(2, Vec2::ZERO); // cheater

        let mut honest = ClientInput {
            seq: 1, tick: 1, move_dir: Vec2::new(1.0, 0.0), aim: Vec2::new(1.0, 0.0), shoot: false,
        };
        let v1 = pipe.run(&GuardCtx { tick: 1, player: 1 }, &mut honest);
        assert_eq!(v1, GuardVerdict::Ok { anomaly: false });
        sim.apply_move(1, honest.move_dir);

        let mut cheat = ClientInput {
            seq: 1, tick: 1, move_dir: Vec2::new(10.0, 0.0), aim: Vec2::new(1.0, 0.0), shoot: false,
        };
        let v2 = pipe.run(&GuardCtx { tick: 1, player: 2 }, &mut cheat);
        assert_eq!(v2, GuardVerdict::Ok { anomaly: true }); // clamp flagged for the detector
        sim.apply_move(2, cheat.move_dir);

        assert_eq!(sim.player(1).unwrap().pos, sim.player(2).unwrap().pos);
    }

    /// A flood of inputs in one tick advances the player exactly once: the sim
    /// only ever folds the first accepted input; input_rate rejects the rest.
    #[test]
    fn input_flood_advances_only_one_tick_of_movement() {
        let mut pipe = Pipeline::standard();
        let mut sim = Sim::new();
        sim.spawn(1, Vec2::ZERO);

        let ctx = GuardCtx { tick: 1, player: 1 };
        let mut accepted = 0;
        for seq in 1..=100 {
            let mut i = ClientInput {
                seq, tick: 1, move_dir: Vec2::new(1.0, 0.0), aim: Vec2::ZERO, shoot: false,
            };
            if let GuardVerdict::Ok { .. } = pipe.run(&ctx, &mut i) {
                sim.apply_move(1, i.move_dir);
                accepted += 1;
            }
        }
        assert_eq!(accepted, 1);
        assert_eq!(sim.player(1).unwrap().pos, Vec2::new(sim::MOVE_SPEED, 0.0));
    }
}
