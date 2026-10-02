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
        // Refuse a duplicated pair before anything else. Deduplicating would
        // make total_tasks disagree with the request, and would diverge from
        // the PostgreSQL adapter, whose unique constraint collapses it.
        let mut seen = BTreeSet::new();
        for spec in &request.tasks {
            if !seen.insert((spec.tile, spec.task_type)) {
                return Err(TaskStoreError::DuplicateTask {
                    tile: spec.tile,
                    task_type: spec.task_type,
                });
            }
        }

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

    async fn claim(&self, request: ClaimRequest) -> Result<Option<ClaimedTask>, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        let wanted = request.task_types.as_deref();
        let next = state
            .tasks
            .iter()
            .filter(|(_, task)| task.state == TaskState::Pending && task.claimable_at <= now)
            .filter(|(_, task)| wanted.map_or(true, |types| types.contains(&task.task_type)))
            .min_by_key(|(id, task)| (task.claimable_at, **id))
            .map(|(id, _)| *id);

        let Some(task_id) = next else {
            return Ok(None);
        };

        let token = LeaseToken::generate();
        let task = state
            .tasks
            .get_mut(&task_id)
            .expect("the id was just selected from this map");
        task.state = TaskState::Claimed;
        task.attempts += 1;
        task.lease = Some(token);
        task.claimed_by = Some(request.worker);
        task.claimed_at = Some(now);
        task.last_heartbeat_at = Some(now);

        Ok(Some(ClaimedTask {
            task_id,
            job_id: task.job_id,
            lease: token,
            tile: task.tile,
            task_type: task.task_type,
            attempt: task.attempts,
        }))
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
    async fn creating_a_job_reports_what_it_created() {
        let store = store(clock());
        let created = store.create_job(two_tile_job()).await.expect("create");
        assert!(created.created);
        assert_eq!(created.total_tasks, 2);
    }

    #[tokio::test]
    async fn creating_the_same_job_again_resumes_rather_than_duplicating() {
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
    async fn two_revisions_of_one_region_are_separate_jobs() {
        let store = store(clock());
        let first = store.create_job(two_tile_job()).await.expect("create");

        let mut next = two_tile_job();
        next.revision = 2;
        let second = store.create_job(next).await.expect("create");

        assert_ne!(first.job_id, second.job_id);
        assert!(second.created);
    }

    #[tokio::test]
    async fn a_task_set_with_a_duplicate_tile_and_task_type_is_refused() {
        let store = store(clock());
        let mut duped = two_tile_job();
        // Push a duplicate of the first task
        duped.tasks.push(TaskSpec {
            tile: tile(50, -2),
            task_type: TaskType::Ortho,
        });

        let error = store
            .create_job(duped)
            .await
            .expect_err("should reject duplicate");
        assert!(
            matches!(error, TaskStoreError::DuplicateTask { .. }),
            "expected DuplicateTask, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_same_tile_with_different_task_types_is_still_accepted() {
        let store = store(clock());
        // two_tile_job already has the same tile with both Ortho and Overlay
        let created = store
            .create_job(two_tile_job())
            .await
            .expect("should accept different task types on same tile");
        assert!(created.created);
        assert_eq!(created.total_tasks, 2);
    }

    fn any(worker: &str) -> ClaimRequest {
        ClaimRequest {
            worker: worker.to_string(),
            task_types: None,
        }
    }

    #[tokio::test]
    async fn a_claim_hands_out_one_task_with_a_lease() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let claimed = store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a task was available");
        assert_eq!(claimed.attempt, 1);
    }

    #[tokio::test]
    async fn each_claim_mints_a_distinct_lease_token() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        let second = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(first.task_id, second.task_id);
        assert_ne!(first.lease, second.lease);
    }

    #[tokio::test]
    async fn a_claimed_task_is_not_handed_out_again() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        store.claim(any("pod-1")).await.unwrap().unwrap();
        store.claim(any("pod-2")).await.unwrap().unwrap();
        let third = store.claim(any("pod-3")).await.unwrap();
        assert!(
            third.is_none(),
            "only two tasks exist, so the third claim finds none"
        );
    }

    #[tokio::test]
    async fn an_empty_queue_is_not_an_error() {
        let store = store(clock());
        assert!(store.claim(any("pod-1")).await.expect("claim").is_none());
    }

    #[tokio::test]
    async fn a_task_type_filter_restricts_what_is_handed_out() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.expect("create");

        let claimed = store
            .claim(ClaimRequest {
                worker: "pod-1".to_string(),
                task_types: Some(vec![TaskType::Overlay]),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.task_type, TaskType::Overlay);

        let again = store
            .claim(ClaimRequest {
                worker: "pod-2".to_string(),
                task_types: Some(vec![TaskType::Overlay]),
            })
            .await
            .unwrap();
        assert!(again.is_none(), "only one overlay task exists");
    }

    #[tokio::test]
    async fn a_task_is_not_claimable_before_its_backoff_elapses() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.expect("create");

        // Claim and fail one task so it is requeued with backoff.
        let claimed = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    task_id: claimed.task_id,
                    token: claimed.lease,
                },
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");

        // The other task is still claimable; take it out of the way.
        store.claim(any("pod-2")).await.unwrap().unwrap();

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "the failed task is in backoff and must not be claimable yet"
        );

        test_clock.advance(Duration::from_secs(60));
        let after_backoff = store.claim(any("pod-3")).await.unwrap();
        assert!(
            after_backoff.is_some(),
            "backoff has elapsed, so it is claimable"
        );
        assert_eq!(after_backoff.unwrap().attempt, 2, "a second start");
    }

    #[tokio::test]
    async fn claims_are_handed_out_oldest_claimable_first() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.expect("create");

        // Fail the first task so it is requeued to a later claimable_at,
        // leaving the second task strictly older.
        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        store
            .fail(FailRequest {
                lease: Lease {
                    task_id: first.task_id,
                    token: first.lease,
                },
                reason: "transient".to_string(),
            })
            .await
            .expect("fail");

        let next = store.claim(any("pod-2")).await.unwrap().unwrap();
        assert_ne!(
            next.task_id, first.task_id,
            "the task still at its original claimable_at must come first"
        );
    }
}
