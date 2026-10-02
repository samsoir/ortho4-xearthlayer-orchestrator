use async_trait::async_trait;

use crate::error::TaskStoreError;
use crate::ids::JobId;
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobCreated, JobStatus, Lease,
    ReapOutcome, ReapRequest, Throughput,
};

/// Durable task state, leasing, retry accounting and the completion gate.
///
/// Every method takes an owned request and returns an owned response, and
/// no transaction spans a call. That is deliberate: it keeps a network
/// adapter a mechanical addition rather than a redesign, and it rules out
/// exposing `begin → select → update → commit` across this boundary.
///
/// `async_trait` is used so the port is dyn-compatible — the composition
/// root injects an adapter behind a trait object.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Register a job and its task set. Idempotent on
    /// `(region_code, revision)`: calling it again with the same task set
    /// and policy resumes the existing job and reports `created: false`.
    /// Calling it with the same identity but a different task set or policy
    /// is [`TaskStoreError::JobConflict`].
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError>;

    /// Hand one claimable task to a worker, minting a fresh lease token.
    /// `Ok(None)` means nothing is claimable, which is not an error.
    ///
    /// Increments the task's attempt count. Attempts count starts, so a task
    /// reclaimed from a dead worker has already consumed one.
    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError>;

    /// Assert that a lease is still held. Returns
    /// [`TaskStoreError::LeaseLost`] if the task was reclaimed, which tells
    /// the worker to stop working.
    async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError>;

    /// Mark a claimed task succeeded.
    async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError>;

    /// Record a failure, requeueing after backoff or abandoning if the
    /// attempt budget is spent.
    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError>;

    /// Reclaim claimed tasks whose heartbeat has lapsed or which have
    /// exceeded the maximum duration. A reaped task takes the same backoff
    /// as a reported failure, so a tile that kills its worker cannot be
    /// re-claimed instantly and burn its budget in minutes.
    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError>;

    /// The completion gate.
    async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError>;

    /// A snapshot of the job's queue.
    async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composition root injects an adapter behind a trait object, so
    /// losing dyn-compatibility would break the design. This fails to
    /// compile rather than at runtime if that happens.
    #[test]
    fn the_port_is_dyn_compatible() {
        fn assert_dyn_compatible(_: Option<&dyn TaskStore>) {}
        assert_dyn_compatible(None);
    }
}
