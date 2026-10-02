//! PostgreSQL adapter for the OXO task store.
//!
//! Claiming uses `SELECT … FOR UPDATE SKIP LOCKED`, PostgreSQL's own idiom
//! for handing one row to exactly one of many concurrent consumers.
//!
//! Every timestamp arrives as a query parameter from the injected
//! [`Clock`](oxo_tasks::Clock). This adapter never calls SQL `now()`: that
//! would put the application and the database on separate clocks, so skew
//! between them would silently change when a task is reclaimed, and it would
//! make expiry testable only by sleeping.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use oxo_tasks::clock::Clock;
use oxo_tasks::error::TaskStoreError;
use oxo_tasks::ids::JobId;
use oxo_tasks::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobCreated, JobStatus, Lease,
    ReapOutcome, ReapRequest, Throughput,
};
use oxo_tasks::store::TaskStore;
use sqlx::PgPool;

/// Apply the schema. Idempotent; safe to call on every start.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// A task store backed by PostgreSQL.
///
/// The fields are unread until Task 10 fills the first port methods, and
/// `make lint` runs clippy with `-D warnings` over the whole workspace, so
/// the allow is load-bearing until then. It mirrors the same allow on the
/// in-memory adapter's `Task` struct.
#[derive(Clone)]
#[allow(dead_code)]
pub struct PostgresTaskStore {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl PostgresTaskStore {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }
}

/// Adapter failures become `TaskStoreError::Adapter`, which is the only
/// variant a caller may consider retrying.
///
/// Unused until Task 10, for the same reason the struct carries an allow.
#[allow(dead_code)]
fn adapter(error: sqlx::Error) -> TaskStoreError {
    TaskStoreError::Adapter(error.to_string())
}

#[async_trait]
impl TaskStore for PostgresTaskStore {
    async fn create_job(&self, _request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        unimplemented!("Task 10")
    }
    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        unimplemented!("Task 10")
    }
    async fn heartbeat(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn complete(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        unimplemented!("Task 11")
    }
    async fn job_status(&self, _job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        unimplemented!("Task 12")
    }
    async fn throughput(&self, _job_id: JobId) -> Result<Throughput, TaskStoreError> {
        unimplemented!("Task 12")
    }
}
