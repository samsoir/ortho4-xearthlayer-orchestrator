use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Source of time for the task store.
///
/// Injected rather than read from the host so that expiry, backoff and the
/// maximum-duration backstop are deterministically testable, and so the
/// application and its database never disagree about the current instant.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The host clock. The only place in this crate that reads real time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock a test drives by hand. Never moves unless advanced.
#[derive(Debug)]
pub struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// Move time forward. Panics only if a previous holder of the lock
    /// panicked, which in a test is the failure you want surfaced.
    pub fn advance(&self, by: Duration) {
        let step = chrono::Duration::from_std(by).expect("advance fits in chrono::Duration");
        let mut now = self.now.lock().expect("test clock lock poisoned");
        *now += step;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("test clock lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;

    #[test]
    fn a_test_clock_does_not_move_on_its_own() {
        let clock = TestClock::new(Utc.timestamp_opt(1_700_000_000, 0).unwrap());
        let first = clock.now();
        let second = clock.now();
        assert_eq!(first, second);
    }

    #[test]
    fn a_test_clock_advances_exactly_as_asked() {
        let start = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let clock = TestClock::new(start);
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now(), start + chrono::Duration::seconds(90));
    }

    #[test]
    fn the_system_clock_moves_forward() {
        let clock = SystemClock;
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
    }
}
