use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::clock::Clock;
use crate::error::TaskStoreError;
use crate::ids::{JobId, LeaseToken, TaskId};
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobCreated, JobStatus, Lease,
    ReapOutcome, ReapRequest, TaskSpec, Throughput,
};
use crate::store::TaskStore;
use crate::task::{TaskState, TaskType};

/// A task store held entirely in memory.
///
/// Exists so the port can be exercised without a database, and so the
/// conformance suite has a second implementation to hold the PostgreSQL
/// adapter honest. Not durable; not intended for production.
///
/// The mutex is never held across an await — every operation is synchronous
/// once inside it — so a `std` mutex is correct here and simpler than an
/// async one.
pub struct InMemoryTaskStore {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl std::fmt::Debug for InMemoryTaskStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryTaskStore")
            .field("state", &self.state)
            .finish()
    }
}

#[derive(Debug, Default)]
struct State {
    jobs: BTreeMap<JobId, Job>,
    by_identity: BTreeMap<(String, u32), JobId>,
    tasks: BTreeMap<TaskId, Task>,
}

#[derive(Debug)]
struct Job {
    max_attempts: u32,
    backoff: Duration,
    task_ids: Vec<TaskId>,
}

#[derive(Debug)]
#[allow(dead_code)]
struct Task {
    job_id: JobId,
    tile: TileId,
    task_type: TaskType,
    state: TaskState,
    attempts: u32,
    claimable_at: DateTime<Utc>,
    lease: Option<LeaseToken>,
    claimed_by: Option<String>,
    claimed_at: Option<DateTime<Utc>>,
    last_heartbeat_at: Option<DateTime<Utc>>,
    last_failure: Option<String>,
}

impl InMemoryTaskStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: Mutex::new(State::default()),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("in-memory store lock poisoned")
    }
}

/// The identity of a task within its job, used to compare task sets.
fn task_key(spec: &TaskSpec) -> (TileId, TaskType) {
    (spec.tile, spec.task_type)
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let identity = (request.region_code.clone(), request.revision);

        if let Some(&job_id) = state.by_identity.get(&identity) {
            let job = &state.jobs[&job_id];
            let existing: BTreeSet<(TileId, TaskType)> = job
                .task_ids
                .iter()
                .map(|id| {
                    let task = &state.tasks[id];
                    (task.tile, task.task_type)
                })
                .collect();
            let requested: BTreeSet<(TileId, TaskType)> =
                request.tasks.iter().map(task_key).collect();

            let same_policy =
                job.max_attempts == request.max_attempts && job.backoff == request.backoff;
            if existing != requested || !same_policy {
                return Err(TaskStoreError::JobConflict {
                    region_code: request.region_code,
                    revision: request.revision,
                });
            }

            return Ok(JobCreated {
                job_id,
                created: false,
                total_tasks: job.task_ids.len() as u32,
            });
        }

        let job_id = JobId::generate();
        let mut task_ids = Vec::with_capacity(request.tasks.len());
        for spec in &request.tasks {
            let task_id = TaskId::generate();
            state.tasks.insert(
                task_id,
                Task {
                    job_id,
                    tile: spec.tile,
                    task_type: spec.task_type,
                    state: TaskState::Pending,
                    attempts: 0,
                    claimable_at: now,
                    lease: None,
                    claimed_by: None,
                    claimed_at: None,
                    last_heartbeat_at: None,
                    last_failure: None,
                },
            );
            task_ids.push(task_id);
        }
        let total_tasks = task_ids.len() as u32;
        state.jobs.insert(
            job_id,
            Job {
                max_attempts: request.max_attempts,
                backoff: request.backoff,
                task_ids,
            },
        );
        state.by_identity.insert(identity, job_id);

        Ok(JobCreated {
            job_id,
            created: true,
            total_tasks,
        })
    }

    async fn claim(&self, _request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        unimplemented!("Task 5")
    }

    async fn heartbeat(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn complete(&self, _lease: Lease) -> Result<(), TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn fail(&self, _request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        unimplemented!("Task 6")
    }

    async fn reap_expired(&self, _request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        unimplemented!("Task 7")
    }

    async fn job_status(&self, _job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        unimplemented!("Task 7")
    }

    async fn throughput(&self, _job_id: JobId) -> Result<Throughput, TaskStoreError> {
        unimplemented!("Task 7")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use chrono::TimeZone;

    fn tile(lat: i8, lon: i16) -> TileId {
        TileId::new(lat, lon).expect("in range")
    }

    pub(crate) fn clock() -> Arc<TestClock> {
        Arc::new(TestClock::new(Utc.timestamp_opt(1_700_000_000, 0).unwrap()))
    }

    pub(crate) fn store(clock: Arc<TestClock>) -> InMemoryTaskStore {
        InMemoryTaskStore::new(clock)
    }

    pub(crate) fn two_tile_job() -> CreateJob {
        CreateJob {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: 3,
            backoff: Duration::from_secs(60),
            tasks: vec![
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Ortho,
                },
                TaskSpec {
                    tile: tile(50, -2),
                    task_type: TaskType::Overlay,
                },
            ],
        }
    }

    #[tokio::test]
    async fn creating_a_run_reports_what_it_created() {
        let store = store(clock());
        let created = store.create_job(two_tile_job()).await.expect("create");
        assert!(created.created);
        assert_eq!(created.total_tasks, 2);
    }

    #[tokio::test]
    async fn creating_the_same_run_again_resumes_rather_than_duplicating() {
        let store = store(clock());
        let first = store.create_job(two_tile_job()).await.expect("create");
        let second = store.create_job(two_tile_job()).await.expect("resume");

        assert!(first.created);
        assert!(
            !second.created,
            "the second call must not have created anything"
        );
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(second.total_tasks, 2);
    }

    #[tokio::test]
    async fn a_different_task_set_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut altered = two_tile_job();
        altered.tasks.push(TaskSpec {
            tile: tile(51, -2),
            task_type: TaskType::Ortho,
        });

        let error = store
            .create_job(altered)
            .await
            .expect_err("should conflict");
        assert!(
            matches!(error, TaskStoreError::JobConflict { .. }),
            "expected JobConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_different_failure_policy_under_the_same_identity_is_a_conflict() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut altered = two_tile_job();
        altered.max_attempts = 5;

        let error = store
            .create_job(altered)
            .await
            .expect_err("should conflict");
        assert!(
            matches!(error, TaskStoreError::JobConflict { .. }),
            "expected JobConflict, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_task_set_is_compared_without_regard_to_order() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let mut reordered = two_tile_job();
        reordered.tasks.reverse();

        let resumed = store.create_job(reordered).await.expect("resume");
        assert!(
            !resumed.created,
            "reordering the task set is not a different job"
        );
    }

    #[tokio::test]
    async fn two_revisions_of_one_region_are_separate_runs() {
        let store = store(clock());
        let first = store.create_job(two_tile_job()).await.expect("create");

        let mut next = two_tile_job();
        next.revision = 2;
        let second = store.create_job(next).await.expect("create");

        assert_ne!(first.job_id, second.job_id);
        assert!(second.created);
    }
}
