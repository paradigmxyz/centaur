//! The console's source of the current time: the system clock, or a fixed
//! one tests move by hand.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Duration, Utc};

#[derive(Clone)]
pub struct Clock(Arc<ClockSource>);

enum ClockSource {
    System,
    Fixed(Mutex<DateTime<Utc>>),
}

impl Clock {
    pub fn system() -> Self {
        Self(Arc::new(ClockSource::System))
    }

    /// A clock frozen at `now` until [`Clock::set`] or [`Clock::advance`].
    pub fn fixed(now: DateTime<Utc>) -> Self {
        Self(Arc::new(ClockSource::Fixed(Mutex::new(now))))
    }

    pub fn now(&self) -> DateTime<Utc> {
        match &*self.0 {
            ClockSource::System => Utc::now(),
            ClockSource::Fixed(now) => *now.lock().expect("clock lock"),
        }
    }

    /// Moves a fixed clock to `now`. Panics on the system clock.
    pub fn set(&self, now: DateTime<Utc>) {
        match &*self.0 {
            ClockSource::System => panic!("only a fixed clock can be set"),
            ClockSource::Fixed(current) => *current.lock().expect("clock lock") = now,
        }
    }

    /// Advances a fixed clock. Panics on the system clock.
    pub fn advance(&self, duration: Duration) {
        self.set(self.now() + duration);
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            ClockSource::System => formatter.write_str("Clock::System"),
            ClockSource::Fixed(now) => write!(formatter, "Clock::Fixed({:?})", now.lock()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_clock_moves_only_when_told() {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let clock = Clock::fixed(start);
        assert_eq!(clock.now(), start);
        clock.advance(Duration::seconds(60));
        assert_eq!(clock.now(), start + Duration::seconds(60));
        clock.set(start);
        assert_eq!(clock.now(), start);
    }
}
