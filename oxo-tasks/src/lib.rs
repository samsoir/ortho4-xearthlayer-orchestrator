//! Task store port, state machine and in-memory adapter for the Ortho4
//! XEarthLayer Orchestrator.
//!
//! This crate has no database dependency. Consumers depend on the
//! [`TaskStore`] port; a concrete adapter is named only at the composition
//! root. Every timestamp comes from an injected [`Clock`], so expiry and
//! backoff are deterministically testable and the application never
//! disagrees with its database about the current instant.

#![forbid(unsafe_code)]

pub mod clock;
pub mod error;
pub mod ids;
pub mod request;
pub mod task;

pub use clock::{Clock, SystemClock, TestClock};
pub use error::TaskStoreError;
pub use ids::{JobId, LeaseToken, TaskId};
pub use request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobCreated, JobStatus, Lease,
    ReapOutcome, ReapRequest, TaskSpec, Throughput,
};
pub use task::{TaskState, TaskType};
