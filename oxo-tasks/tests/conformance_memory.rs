//! The conformance suite, run against the in-memory adapter.
//!
//! The same cases run against `oxo-tasks-postgres`. If one adapter passes a
//! case the other fails, the two have diverged — which is the whole reason
//! this suite exists.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use oxo_tasks::clock::TestClock;
use oxo_tasks::conformance::{Fixture, Subject};
use oxo_tasks::InMemoryTaskStore;

struct Memory;

#[async_trait]
impl Fixture for Memory {
    async fn fresh(&self) -> Subject {
        let clock = Arc::new(TestClock::new(
            chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ));
        Subject {
            store: Box::new(InMemoryTaskStore::new(clock.clone())),
            clock,
        }
    }
}

oxo_tasks::conformance_suite!(Memory);
