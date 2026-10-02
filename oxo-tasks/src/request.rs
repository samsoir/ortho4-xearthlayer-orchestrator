use chrono::{DateTime, Utc};
use oxo_spec::TileId;

use crate::ids::{JobId, LeaseToken, TaskId};
use crate::quantity::{BackoffSeconds, MaxAttempts, TimeoutSeconds};
use crate::task::TaskType;

/// One unit of work within a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub tile: TileId,
    pub task_type: TaskType,
}

/// Register a job and the whole task set it consists of.
///
/// The task set arrives already computed: atomizing a specification into up
/// to 2N tasks is the planner's work, not the store's. The failure policy is
/// snapshotted onto the job, so editing a specification later cannot change
/// the policy of a job already in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJob {
    pub region_code: String,
    pub revision: u32,
    pub max_attempts: MaxAttempts,
    pub backoff: BackoffSeconds,
    pub tasks: Vec<TaskSpec>,
}

/// The outcome of registering a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobCreated {
    pub job_id: JobId,
    /// `false` when a job for this identity already existed and this call
    /// resumed it rather than creating anything.
    pub created: bool,
    pub total_tasks: u32,
}

/// Ask for one task to work on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// Worker identity, recorded so an operator can see who holds what.
    pub worker: String,
    /// Restrict to these task types. `None` means any.
    ///
    /// `Some(vec![])` matches nothing — an empty capacity set means no
    /// capacity — as distinct from `None`, which matches anything. This is
    /// how a worker expresses capacity until there is a footprint model: one
    /// short on disk claims overlay work only, an overlay task being a file
    /// copy and a conversion where an ortho task is hundreds of gigabytes.
    pub task_types: Option<Vec<TaskType>>,
}

/// A task handed to a worker, with the lease that proves it is theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedTask {
    pub task_id: TaskId,
    pub job_id: JobId,
    pub lease: LeaseToken,
    pub tile: TileId,
    pub task_type: TaskType,
    /// Which start this is, counting from 1.
    pub attempt: u32,
}

/// Proof that the caller holds a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub task_id: TaskId,
    pub token: LeaseToken,
}

/// Report that a task failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailRequest {
    pub lease: Lease,
    /// Whatever the worker could determine. Recorded, never interpreted —
    /// Ortho4XP's headless path reports a bare `Crash!`, so the store draws
    /// no conclusions from this text.
    pub reason: String,
}

/// What became of a failed task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    Requeued {
        claimable_at: DateTime<Utc>,
        attempts_remaining: u32,
    },
    Abandoned,
}

/// Reclaim tasks whose workers have gone away.
///
/// Both bounds are server configuration rather than per-job or
/// per-specification: they are operational tuning, and an operator
/// authoring a region is the person least placed to choose them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapRequest {
    /// Reclaim a claimed task if no heartbeat has arrived within this.
    pub heartbeat_timeout: TimeoutSeconds,
    /// Reclaim a claimed task once it has been held this long regardless of
    /// heartbeats, covering a worker that is wedged but alive.
    pub max_task_duration: TimeoutSeconds,
}

/// What a reap did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReapOutcome {
    pub requeued: u32,
    /// Reclaimed tasks whose attempts were already exhausted.
    pub abandoned: u32,
}

/// Whether a region is finished — the completion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Complete,
    /// Nothing left to run, and at least one task abandoned. The region can
    /// never complete.
    Failed {
        abandoned: u32,
    },
    /// Work remains. `abandoned` is reported here too, deliberately: a job
    /// with an abandoned task is already unachievable, and an operator should
    /// learn that in minutes rather than after a fortnight.
    InProgress {
        pending: u32,
        claimed: u32,
        succeeded: u32,
        abandoned: u32,
    },
}

/// A snapshot of a job's queue.
///
/// Counts, not rates: rates need two observations, and differencing
/// successive snapshots is the consumer's task. The metric set is an open
/// decision in the design document, to be settled with the first real
/// consumer rather than guessed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Throughput {
    pub pending: u32,
    /// Pending tasks whose backoff has elapsed, so claimable right now.
    pub claimable_now: u32,
    pub claimed: u32,
    pub succeeded: u32,
    pub abandoned: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(lat: i8, lon: i16) -> TileId {
        TileId::new(lat, lon).expect("in range")
    }

    #[test]
    fn a_create_job_request_carries_its_whole_task_set() {
        let request = CreateJob {
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
        };
        assert_eq!(request.tasks.len(), 2);
        assert_eq!(request.tasks[0].tile, request.tasks[1].tile);
        assert_ne!(request.tasks[0].task_type, request.tasks[1].task_type);
    }

    // Only one test here, and deliberately so. These are plain data
    // definitions, so the red-green driver is compilation: before the types
    // exist this module does not compile, and after they do it does. Three
    // further tests were drafted and removed during the pre-flight scan —
    // two asserted that distinct enum variants differ, which can never
    // fail, one asserted that a field just set to `None` is `None`, and one
    // of them called `Utc::now()` directly, which the Global Constraints
    // forbid. Behavioural coverage of every one of these types arrives in
    // Tasks 4 through 8, which assert concrete values such as
    // `FailOutcome::Abandoned` and `JobStatus::Complete`.
}
