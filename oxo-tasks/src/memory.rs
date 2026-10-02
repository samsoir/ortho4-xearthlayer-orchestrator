use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::clock::Clock;
use crate::error::TaskStoreError;
use crate::ids::{JobId, LeaseToken, TaskId};
use crate::quantity::BackoffSeconds;
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, FindJob, JobCreated, JobStatus,
    Lease, ReapOutcome, ReapRequest, TaskSpec, Throughput,
};
use crate::store::TaskStore;
use crate::task::{TaskState, TaskType};

/// Convert a bounded quantity of seconds to a `chrono` duration.
fn chrono_seconds(seconds: u64) -> chrono::Duration {
    chrono::Duration::seconds(
        i64::try_from(seconds).expect("bounded by MAX_SECONDS at construction"),
    )
}

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
    backoff: BackoffSeconds,
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

impl State {
    /// Resolve a lease to a claimed task, rejecting the three ways it can be
    /// invalid. Shared by heartbeat, complete and fail so the checks cannot
    /// drift apart between them.
    fn claimed_mut(&mut self, lease: Lease) -> Result<&mut Task, TaskStoreError> {
        let task = self
            .tasks
            .get_mut(&lease.task_id)
            .ok_or(TaskStoreError::UnknownTask {
                task_id: lease.task_id,
            })?;
        if task.state != TaskState::Claimed {
            return Err(TaskStoreError::NotClaimed {
                task_id: lease.task_id,
            });
        }
        if task.lease != Some(lease.token) {
            return Err(TaskStoreError::LeaseLost {
                task_id: lease.task_id,
            });
        }
        Ok(task)
    }

    /// Count tasks per state for one job. Shared by the gate and the
    /// throughput snapshot so the two cannot disagree.
    fn tally(&self, job_id: JobId, now: DateTime<Utc>) -> Result<Tally, TaskStoreError> {
        let job = self
            .jobs
            .get(&job_id)
            .ok_or(TaskStoreError::UnknownJob { job_id })?;
        let mut tally = Tally::default();
        for id in &job.task_ids {
            let task = self
                .tasks
                .get(id)
                .expect("job.task_ids only contains ids present in state.tasks");
            match task.state {
                TaskState::Pending => {
                    tally.pending += 1;
                    if task.claimable_at <= now {
                        tally.claimable_now += 1;
                    }
                }
                TaskState::Claimed => tally.claimed += 1,
                TaskState::Succeeded => tally.succeeded += 1,
                TaskState::Abandoned => tally.abandoned += 1,
            }
        }
        Ok(tally)
    }
}

#[derive(Debug, Default)]
struct Tally {
    pending: u32,
    claimable_now: u32,
    claimed: u32,
    succeeded: u32,
    abandoned: u32,
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
        if request.tasks.is_empty() {
            return Err(TaskStoreError::EmptyJob {
                region_code: request.region_code,
                revision: request.revision,
            });
        }

        // Refuse duplicate pairs after the empty check. Deduplicating would
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
                job.max_attempts == request.max_attempts.get() && job.backoff == request.backoff;
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
                max_attempts: request.max_attempts.get(),
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

    async fn find_job(&self, request: FindJob) -> Result<Option<JobId>, TaskStoreError> {
        let state = self.locked();
        Ok(state
            .by_identity
            .get(&(request.region_code, request.revision))
            .copied())
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

    async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();
        let task = state.claimed_mut(lease)?;
        task.last_heartbeat_at = Some(now);
        Ok(())
    }

    async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError> {
        let mut state = self.locked();
        let task = state.claimed_mut(lease)?;
        task.state = TaskState::Succeeded;
        task.lease = None;
        task.claimed_by = None;
        task.claimed_at = None;
        task.last_heartbeat_at = None;
        Ok(())
    }

    async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
        let now = self.clock.now();
        let mut state = self.locked();

        // The job's policy is needed before the task is mutably borrowed.
        let job_id = state
            .tasks
            .get(&request.lease.task_id)
            .ok_or(TaskStoreError::UnknownTask {
                task_id: request.lease.task_id,
            })?
            .job_id;
        let job = state
            .jobs
            .get(&job_id)
            .ok_or(TaskStoreError::UnknownJob { job_id })?;
        let max_attempts = job.max_attempts;
        let backoff = job.backoff;

        let task = state.claimed_mut(request.lease)?;
        task.lease = None;
        task.claimed_by = None;
        task.claimed_at = None;
        task.last_heartbeat_at = None;
        task.last_failure = Some(request.reason);

        if task.attempts >= max_attempts {
            task.state = TaskState::Abandoned;
            return Ok(FailOutcome::Abandoned);
        }

        let step = chrono_seconds(backoff.get());
        let claimable_at = now + step;
        task.state = TaskState::Pending;
        task.claimable_at = claimable_at;

        Ok(FailOutcome::Requeued {
            claimable_at,
            attempts_remaining: max_attempts - task.attempts,
        })
    }

    async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
        let now = self.clock.now();
        let heartbeat_timeout = chrono_seconds(request.heartbeat_timeout.get());
        let max_duration = chrono_seconds(request.max_task_duration.get());
        let mut state = self.locked();

        // Policies are read before any task is mutably borrowed.
        let policies: BTreeMap<JobId, (u32, BackoffSeconds)> = state
            .jobs
            .iter()
            .map(|(id, job)| (*id, (job.max_attempts, job.backoff)))
            .collect();

        let expired: Vec<TaskId> = state
            .tasks
            .iter()
            .filter(|(_, task)| task.state == TaskState::Claimed)
            .filter(|(_, task)| {
                let heartbeat_lapsed = task
                    .last_heartbeat_at
                    .is_some_and(|last| now - last > heartbeat_timeout);
                let held_too_long = task
                    .claimed_at
                    .is_some_and(|since| now - since > max_duration);
                heartbeat_lapsed || held_too_long
            })
            .map(|(id, _)| *id)
            .collect();

        let mut outcome = ReapOutcome::default();
        for task_id in expired {
            let job_id = state
                .tasks
                .get(&task_id)
                .expect("task_id was selected from state.tasks.iter() above")
                .job_id;
            let (max_attempts, backoff) = *policies
                .get(&job_id)
                .expect("every task.job_id points to a job in state.jobs");
            let task = state.tasks.get_mut(&task_id).expect("just selected");
            task.lease = None;
            task.claimed_by = None;
            task.claimed_at = None;
            task.last_heartbeat_at = None;

            if task.attempts >= max_attempts {
                task.state = TaskState::Abandoned;
                outcome.abandoned += 1;
            } else {
                let step = chrono_seconds(backoff.get());
                task.state = TaskState::Pending;
                task.claimable_at = now + step;
                outcome.requeued += 1;
            }
        }

        Ok(outcome)
    }

    async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(job_id, now)?;

        if tally.pending == 0 && tally.claimed == 0 {
            return Ok(if tally.abandoned == 0 {
                JobStatus::Complete
            } else {
                JobStatus::Failed {
                    abandoned: tally.abandoned,
                }
            });
        }

        Ok(JobStatus::InProgress {
            pending: tally.pending,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }

    async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError> {
        let now = self.clock.now();
        let state = self.locked();
        let tally = state.tally(job_id, now)?;
        Ok(Throughput {
            pending: tally.pending,
            claimable_now: tally.claimable_now,
            claimed: tally.claimed,
            succeeded: tally.succeeded,
            abandoned: tally.abandoned,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::quantity::{MaxAttempts, TimeoutSeconds};
    use chrono::TimeZone;
    use std::time::Duration;

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
            max_attempts: MaxAttempts::new(3).expect("non-zero"),
            backoff: BackoffSeconds::new(60).expect("in range"),
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

    /// A job with exactly one task, for tests about a single task's whole
    /// lifecycle.
    ///
    /// `two_tile_job` has two, and claim order is FIFO by `claimable_at` — so
    /// once a task has failed and been requeued to `now + backoff`, it sorts
    /// *after* a sibling that has never been claimed. A loop that claims "the
    /// next task" then gets the sibling, not the task under test.
    pub(crate) fn one_task_job() -> CreateJob {
        CreateJob {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: MaxAttempts::new(3).expect("non-zero"),
            backoff: BackoffSeconds::new(60).expect("in range"),
            tasks: vec![TaskSpec {
                tile: tile(50, -2),
                task_type: TaskType::Ortho,
            }],
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
        altered.max_attempts = MaxAttempts::new(5).expect("non-zero");

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

    async fn claim_one(store: &InMemoryTaskStore) -> ClaimedTask {
        store
            .claim(any("pod-1"))
            .await
            .expect("claim")
            .expect("a task was available")
    }

    fn lease_of(task: &ClaimedTask) -> Lease {
        Lease {
            task_id: task.task_id,
            token: task.lease,
        }
    }

    fn reap(heartbeat_secs: u64, max_secs: u64) -> ReapRequest {
        ReapRequest {
            heartbeat_timeout: TimeoutSeconds::new(heartbeat_secs).expect("non-zero"),
            max_task_duration: TimeoutSeconds::new(max_secs).expect("non-zero"),
        }
    }

    #[tokio::test]
    async fn a_task_whose_heartbeat_lapses_is_reclaimed_and_takes_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        // Within the timeout: nothing is reclaimed.
        test_clock.advance(Duration::from_secs(60));
        let quiet = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(quiet, ReapOutcome::default());
        store.heartbeat(lease_of(&task)).await.expect("still held");

        // Past the timeout with no further heartbeat: reclaimed.
        test_clock.advance(Duration::from_secs(91));
        let reaped = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(reaped.requeued, 1);
        assert_eq!(reaped.abandoned, 0);

        // The old lease is now worthless.
        let error = store
            .heartbeat(lease_of(&task))
            .await
            .expect_err("reclaimed");
        assert!(
            matches!(
                error,
                TaskStoreError::LeaseLost { .. } | TaskStoreError::NotClaimed { .. }
            ),
            "expected the old lease to be refused, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_reaped_task_is_not_instantly_reclaimable() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        claim_one(&store).await;
        store.claim(any("pod-2")).await.unwrap().unwrap();

        test_clock.advance(Duration::from_secs(100));
        store.reap_expired(reap(90, 86_400)).await.expect("reap");

        assert!(
            store.claim(any("pod-3")).await.unwrap().is_none(),
            "a reaped task takes backoff; re-claiming it instantly would burn its budget in minutes"
        );
        test_clock.advance(Duration::from_secs(60));
        assert!(store.claim(any("pod-3")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_worker_that_heartbeats_forever_is_still_cut_off_by_the_backstop() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        // Heartbeat diligently for well past the maximum duration.
        for _ in 0..10 {
            test_clock.advance(Duration::from_secs(60));
            store
                .heartbeat(lease_of(&task))
                .await
                .expect("a held lease heartbeats");
        }

        let reaped = store.reap_expired(reap(90, 300)).await.expect("reap");
        assert_eq!(
            reaped.requeued, 1,
            "a wedged-but-alive worker must not hold a task indefinitely"
        );
    }

    #[tokio::test]
    async fn a_reap_consumes_an_attempt_and_can_abandon() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut job = two_tile_job();
        job.max_attempts = MaxAttempts::new(1).expect("non-zero");
        store.create_job(job).await.unwrap();

        claim_one(&store).await;
        test_clock.advance(Duration::from_secs(100));

        let reaped = store.reap_expired(reap(90, 86_400)).await.expect("reap");
        assert_eq!(
            (reaped.requeued, reaped.abandoned),
            (0, 1),
            "the single permitted start was spent by claiming, so the reap abandons"
        );
    }

    #[tokio::test]
    async fn the_gate_reports_in_progress_then_complete() {
        let store = store(clock());
        let job = store.create_job(two_tile_job()).await.unwrap();

        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::InProgress {
                pending: 2,
                claimed: 0,
                succeeded: 0,
                abandoned: 0
            }
        );

        for _ in 0..2 {
            let task = claim_one(&store).await;
            store.complete(lease_of(&task)).await.unwrap();
        }

        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::Complete
        );
    }

    #[tokio::test]
    async fn an_abandoned_task_is_visible_while_work_continues_then_fails_the_job() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let mut spec = two_tile_job();
        spec.max_attempts = MaxAttempts::new(1).expect("non-zero");
        let job = store.create_job(spec).await.unwrap();

        let doomed = claim_one(&store).await;
        store
            .fail(FailRequest {
                lease: lease_of(&doomed),
                reason: "Crash!".to_string(),
            })
            .await
            .unwrap();

        // One tile is unrecoverable, but the other is still runnable — and
        // the operator can see the problem now rather than in a fortnight.
        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::InProgress {
                pending: 1,
                claimed: 0,
                succeeded: 0,
                abandoned: 1
            }
        );

        let other = claim_one(&store).await;
        store.complete(lease_of(&other)).await.unwrap();

        assert_eq!(
            store.job_status(job.job_id).await.unwrap(),
            JobStatus::Failed { abandoned: 1 }
        );
    }

    #[tokio::test]
    async fn the_gate_rejects_a_job_it_does_not_know() {
        let store = store(clock());
        let error = store
            .job_status(JobId::generate())
            .await
            .expect_err("unknown job");
        assert!(
            matches!(error, TaskStoreError::UnknownJob { .. }),
            "expected UnknownJob, got {error:?}"
        );
    }

    #[tokio::test]
    async fn throughput_separates_pending_from_claimable_now() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        let job = store.create_job(two_tile_job()).await.unwrap();

        let task = claim_one(&store).await;
        store
            .fail(FailRequest {
                lease: lease_of(&task),
                reason: "Crash!".to_string(),
            })
            .await
            .unwrap();

        let snapshot = store.throughput(job.job_id).await.unwrap();
        assert_eq!(snapshot.pending, 2, "both tasks are pending");
        assert_eq!(
            snapshot.claimable_now, 1,
            "one is in backoff, so only one can be claimed right now"
        );

        test_clock.advance(Duration::from_secs(60));
        let later = store.throughput(job.job_id).await.unwrap();
        assert_eq!(later.claimable_now, 2);
    }

    #[tokio::test]
    async fn a_job_with_no_tasks_is_refused_rather_than_declared_complete() {
        let store = store(clock());
        let mut empty = two_tile_job();
        empty.tasks.clear();
        let error = store
            .create_job(empty)
            .await
            .expect_err("an empty job is refused");
        assert!(
            matches!(error, TaskStoreError::EmptyJob { .. }),
            "expected EmptyJob, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_heartbeat_on_a_held_lease_succeeds() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        store.heartbeat(lease_of(&task)).await.expect("still held");
    }

    #[tokio::test]
    async fn completing_a_held_task_succeeds_once_and_not_twice() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;

        store.complete(lease_of(&task)).await.expect("complete");

        let again = store
            .complete(lease_of(&task))
            .await
            .expect_err("already done");
        assert!(
            matches!(again, TaskStoreError::NotClaimed { .. }),
            "expected NotClaimed, got {again:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_lease_is_rejected_by_all_three_reporting_calls() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        let stale = Lease {
            task_id: task.task_id,
            token: LeaseToken::generate(),
        };

        for error in [
            store.heartbeat(stale).await.expect_err("heartbeat"),
            store.complete(stale).await.expect_err("complete"),
            store
                .fail(FailRequest {
                    lease: stale,
                    reason: "Crash!".to_string(),
                })
                .await
                .expect_err("fail"),
        ] {
            assert!(
                matches!(error, TaskStoreError::LeaseLost { .. }),
                "expected LeaseLost, got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn reporting_on_an_unknown_task_says_so() {
        let store = store(clock());
        let nowhere = Lease {
            task_id: TaskId::generate(),
            token: LeaseToken::generate(),
        };
        let error = store.heartbeat(nowhere).await.expect_err("unknown");
        assert!(
            matches!(error, TaskStoreError::UnknownTask { .. }),
            "expected UnknownTask, got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_with_attempts_remaining_is_requeued_with_backoff() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        store.create_job(two_tile_job()).await.unwrap();
        let task = claim_one(&store).await;
        let at_failure = test_clock.now();

        let outcome = store
            .fail(FailRequest {
                lease: lease_of(&task),
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");

        match outcome {
            FailOutcome::Requeued {
                claimable_at,
                attempts_remaining,
            } => {
                assert_eq!(claimable_at, at_failure + chrono::Duration::seconds(60));
                assert_eq!(attempts_remaining, 2, "one of three starts is spent");
            }
            FailOutcome::Abandoned => panic!("two attempts remained"),
        }
    }

    #[tokio::test]
    async fn a_task_is_abandoned_on_the_last_permitted_start() {
        let test_clock = clock();
        let store = store(test_clock.clone());
        // One task, not two: this test follows a single task through its whole
        // attempt budget, and with two tasks the FIFO order would hand out the
        // never-claimed sibling on the second claim.
        store.create_job(one_task_job()).await.unwrap();

        // max_attempts is 3, so the third failure abandons.
        for expected_remaining in [2u32, 1] {
            let task = claim_one(&store).await;
            let outcome = store
                .fail(FailRequest {
                    lease: lease_of(&task),
                    reason: "Crash!".to_string(),
                })
                .await
                .expect("fail");
            assert!(
                matches!(
                    outcome,
                    FailOutcome::Requeued { attempts_remaining, .. }
                        if attempts_remaining == expected_remaining
                ),
                "expected {expected_remaining} remaining, got {outcome:?}"
            );
            test_clock.advance(Duration::from_secs(60));
        }

        let last = claim_one(&store).await;
        assert_eq!(last.attempt, 3, "the third and final start");
        let outcome = store
            .fail(FailRequest {
                lease: lease_of(&last),
                reason: "Crash!".to_string(),
            })
            .await
            .expect("fail");
        assert_eq!(outcome, FailOutcome::Abandoned);
    }

    #[tokio::test]
    async fn an_empty_task_type_filter_matches_nothing() {
        let store = store(clock());
        store.create_job(two_tile_job()).await.unwrap();
        let claimed = store
            .claim(ClaimRequest {
                worker: "pod-1".to_string(),
                task_types: Some(Vec::new()),
            })
            .await
            .expect("claim");
        assert!(
            claimed.is_none(),
            "an empty filter is an empty capacity set, so it must match nothing — \
             not fall through to matching anything"
        );
    }
}
