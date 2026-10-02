use thiserror::Error;

use crate::ids::{JobId, TaskId};
use crate::task::TaskType;
use oxo_spec::TileId;

/// Why a task store operation did not succeed.
///
/// The first five are deterministic outcomes of a correct store and must be
/// representable without a caller parsing a string. Only [`Adapter`] may be
/// worth retrying.
///
/// [`Adapter`]: TaskStoreError::Adapter
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TaskStoreError {
    /// The lease token does not match the one the task was claimed with. The
    /// caller has lost this task — another worker may already hold it — and
    /// must stop rather than retry.
    #[error(
        "lease for task {task_id} is no longer held; it was reclaimed and must not be reported on"
    )]
    LeaseLost { task_id: TaskId },

    #[error("no such job {job_id}")]
    UnknownJob { job_id: JobId },

    #[error("no such task {task_id}")]
    UnknownTask { task_id: TaskId },

    #[error("task {task_id} is not claimed, so it cannot be completed or failed")]
    NotClaimed { task_id: TaskId },

    /// A job for this identity already exists with a different task set or
    /// policy. Resuming it would silently run something other than what was
    /// asked for.
    #[error(
        "a job for region {region_code} revision {revision} already exists with a different task set or failure policy"
    )]
    JobConflict { region_code: String, revision: u32 },

    /// The same tile and task type appeared twice in one job's task set.
    /// Refused rather than deduplicated: collapsing it silently would make
    /// the reported task count disagree with what was asked for, and would
    /// make this adapter disagree with the PostgreSQL one, whose unique
    /// constraint collapses it at the database.
    #[error("task set contains {tile} {task_type} more than once")]
    DuplicateTask { tile: TileId, task_type: TaskType },

    /// A job must contain at least one task. A job of none would report
    /// `Complete` vacuously, which is a silently wrong answer.
    #[error("a job for region {region_code} revision {revision} must contain at least one task")]
    EmptyJob { region_code: String, revision: u32 },

    #[error("task store adapter failed: {0}")]
    Adapter(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;

    #[test]
    fn a_lost_lease_says_so_and_names_the_task() {
        let task_id = TaskId::generate();
        let error = TaskStoreError::LeaseLost { task_id };
        let rendered = error.to_string();
        assert!(rendered.contains("lease"), "{rendered}");
        assert!(rendered.contains(&task_id.to_string()), "{rendered}");
    }

    #[test]
    fn a_job_conflict_names_the_identity_that_collided() {
        let error = TaskStoreError::JobConflict {
            region_code: "NA".to_string(),
            revision: 2,
        };
        let rendered = error.to_string();
        assert!(rendered.contains("NA"), "{rendered}");
        assert!(rendered.contains('2'), "{rendered}");
    }

    #[test]
    fn every_variant_renders_something_an_operator_can_act_on() {
        let task_id = TaskId::generate();
        let job_id = crate::ids::JobId::generate();
        let tile = oxo_spec::TileId::new(50, -2).expect("in range");
        let cases: Vec<(TaskStoreError, &str)> = vec![
            (TaskStoreError::LeaseLost { task_id }, "lease"),
            (TaskStoreError::UnknownJob { job_id }, "job"),
            (TaskStoreError::UnknownTask { task_id }, "task"),
            (TaskStoreError::NotClaimed { task_id }, "not claimed"),
            (
                TaskStoreError::JobConflict {
                    region_code: "OC".to_string(),
                    revision: 1,
                },
                "already exists",
            ),
            (
                TaskStoreError::DuplicateTask {
                    tile,
                    task_type: TaskType::Ortho,
                },
                "more than once",
            ),
            (
                TaskStoreError::EmptyJob {
                    region_code: "NA".to_string(),
                    revision: 1,
                },
                "at least one task",
            ),
            (
                TaskStoreError::Adapter("connection reset".to_string()),
                "connection reset",
            ),
        ];
        for (error, fragment) in cases {
            let rendered = error.to_string();
            assert!(
                rendered.contains(fragment),
                "{error:?} rendered {rendered:?}, expected it to contain {fragment:?}"
            );
        }
    }
}
