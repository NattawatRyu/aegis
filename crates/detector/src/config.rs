//! Every line the detectors draw, in one value a game sets for itself.
//!
//! The defaults ([`Config::DEFAULT`]) are the lab's: measured on bots in a
//! 30 Hz walled arena, each cited in its detector's file. No two games share
//! a tick rate, hitbox size, match length or style of play, so none of them
//! is right everywhere: a game starts from [`Config::at_tick_rate`], replaces
//! what its own honest telemetry contradicts, and must pass
//! [`Config::validate`] before a [`crate::Monitor`] will run it.

use crate::detectors::{accuracy, aim_exact, anomaly_rate, far_aim, foresight, reaction};
use crate::monitor::WINDOW;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Config {
    pub accuracy: accuracy::Config,
    pub aim_exact: aim_exact::Config,
    pub anomaly_rate: anomaly_rate::Config,
    pub reaction: reaction::Config,
    pub foresight: foresight::Config,
    pub far_aim: far_aim::Config,
}

/// Why a config was refused: the field, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: &'static str,
    pub problem: &'static str,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.problem)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub const DEFAULT: Self = Self {
        accuracy: accuracy::Config::DEFAULT,
        aim_exact: aim_exact::Config::DEFAULT,
        anomaly_rate: anomaly_rate::Config::DEFAULT,
        reaction: reaction::Config::DEFAULT,
        foresight: foresight::Config::DEFAULT,
        far_aim: far_aim::Config::DEFAULT,
    };

    /// The defaults for a server ticking `hz` times a second: every line
    /// counted in ticks is converted from the time it stands for.
    pub fn at_tick_rate(hz: u32) -> Self {
        Self { reaction: reaction::Config::at_tick_rate(hz), ..Self::DEFAULT }
    }

    /// Refuse a config that cannot mean what it says. A line nobody can
    /// cross is a detector switched off without anyone deciding to; one
    /// everybody crosses flags every player. Both are refused here, not
    /// discovered on a live server.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let err = |field, problem| Err(ConfigError { field, problem });
        // A share line: flagging is "strictly above", so 1.0 can never fire
        // and a negative one fires on everybody.
        let share = |field, t: f32| if (0.0..1.0).contains(&t) { Ok(()) } else { err(field, "must be in [0, 1)") };
        // A minimum sample: 0 judges a player on nothing; above the window
        // the window view never judges, and a burst goes unseen.
        let min = |field, m: u32| match m {
            0 => err(field, "must be at least 1"),
            m if m as usize > WINDOW => err(field, "must not exceed the monitor's window (100)"),
            _ => Ok(()),
        };
        let angle = |field, r: f32| {
            if r.is_finite() && r > 0.0 && r < std::f32::consts::PI {
                Ok(())
            } else {
                err(field, "must be an angle in (0, pi) radians")
            }
        };
        share("accuracy.threshold", self.accuracy.threshold)?;
        min("accuracy.min_shots", self.accuracy.min_shots)?;
        angle("aim_exact.exact_rad", self.aim_exact.exact_rad)?;
        share("aim_exact.threshold", self.aim_exact.threshold)?;
        min("aim_exact.min_shots", self.aim_exact.min_shots)?;
        share("anomaly_rate.threshold", self.anomaly_rate.threshold)?;
        min("anomaly_rate.min_inputs", self.anomaly_rate.min_inputs)?;
        share("reaction.threshold", self.reaction.threshold)?;
        min("reaction.min_timed", self.reaction.min_timed)?;
        let f = &self.foresight;
        angle("foresight.fit_rad", f.fit_rad)?;
        angle("foresight.clear_rad", f.clear_rad)?;
        if f.clear_rad <= f.fit_rad {
            // Otherwise an aim at a shown enemy, off by less than the hand,
            // reads as unexplained: every honest shot near a new enemy.
            return err("foresight.clear_rad", "must be above foresight.fit_rad");
        }
        share("foresight.threshold", f.threshold)?;
        min("foresight.min_foreseen", f.min_foreseen)?;
        if f.min_foreseen as f32 / WINDOW as f32 <= f.threshold {
            // The window view could then hold min_foreseen and still sit
            // under the share: a burst it should see, it never judges.
            return err("foresight.min_foreseen", "must be above threshold x window, or bursts go unseen");
        }
        // Below pi/2: a target can look no bigger than that, so a line at
        // or above it makes every shot far — raw accuracy, by another name.
        let a = &self.far_aim;
        if !(a.far_rad.is_finite() && a.far_rad > 0.0 && a.far_rad < std::f32::consts::FRAC_PI_2) {
            return err("far_aim.far_rad", "must be an angle in (0, pi/2) radians");
        }
        share("far_aim.threshold", a.threshold)?;
        min("far_aim.min_far", a.min_far)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_at_every_common_tick_rate() {
        assert_eq!(Config::DEFAULT.validate(), Ok(()));
        for hz in [10, 20, 30, 60, 64, 128] {
            assert_eq!(Config::at_tick_rate(hz).validate(), Ok(()), "{hz} Hz");
        }
    }

    #[test]
    fn fast_is_100_ms_whatever_the_tick_rate() {
        let fast = |hz| Config::at_tick_rate(hz).reaction.fast_ticks;
        assert_eq!((fast(30), fast(60), fast(64), fast(128), fast(20), fast(5)), (3, 6, 6, 12, 2, 1));
        assert_eq!(Config::at_tick_rate(30), Config::DEFAULT);
    }

    /// Every check at its edge: the last legal value passes, one step past
    /// it names the field.
    #[test]
    fn each_line_is_refused_just_past_its_edge() {
        let refused = |c: Config| c.validate().unwrap_err().field;
        let d = Config::DEFAULT;
        let mut c = d;
        c.reaction.threshold = 1.0;
        assert_eq!(refused(c), "reaction.threshold");
        c.reaction.threshold = 0.0;
        assert_eq!(c.validate(), Ok(()), "0 is a legal line: any fast reaction flags");
        c.reaction.threshold = -0.01;
        assert_eq!(refused(c), "reaction.threshold");

        let mut c = d;
        c.reaction.min_timed = 0;
        assert_eq!(refused(c), "reaction.min_timed");
        c.reaction.min_timed = WINDOW as u32;
        assert_eq!(c.validate(), Ok(()));
        c.reaction.min_timed = WINDOW as u32 + 1;
        assert_eq!(refused(c), "reaction.min_timed");

        let mut c = d;
        c.aim_exact.exact_rad = f32::NAN;
        assert_eq!(refused(c), "aim_exact.exact_rad");
        c.aim_exact.exact_rad = 0.0;
        assert_eq!(refused(c), "aim_exact.exact_rad");

        let mut c = d;
        c.foresight.clear_rad = c.foresight.fit_rad;
        assert_eq!(refused(c), "foresight.clear_rad");
        c.foresight.clear_rad = c.foresight.fit_rad + 1e-3;
        assert_eq!(c.validate(), Ok(()));

        let mut c = d;
        c.foresight.threshold = 0.03; // 3 in 100 is exactly the share: never above it
        assert_eq!(refused(c), "foresight.min_foreseen");
        c.foresight.min_foreseen = 4;
        assert_eq!(c.validate(), Ok(()));

        let mut c = d;
        c.anomaly_rate.min_inputs = 0;
        assert_eq!(refused(c), "anomaly_rate.min_inputs");
        let mut c = d;
        c.accuracy.threshold = 1.5;
        assert_eq!(refused(c), "accuracy.threshold");

        let mut c = d;
        c.far_aim.far_rad = std::f32::consts::FRAC_PI_2;
        assert_eq!(refused(c), "far_aim.far_rad");
        c.far_aim.far_rad = 1.5;
        assert_eq!(c.validate(), Ok(()));
        c.far_aim.far_rad = 0.0;
        assert_eq!(refused(c), "far_aim.far_rad");
        let mut c = d;
        c.far_aim.threshold = 1.0;
        assert_eq!(refused(c), "far_aim.threshold");
        let mut c = d;
        c.far_aim.min_far = 0;
        assert_eq!(refused(c), "far_aim.min_far");
    }
}
