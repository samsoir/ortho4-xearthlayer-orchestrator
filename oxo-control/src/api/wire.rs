//! Serde DTOs for the claim API. The JSON shapes are the contract a
//! non-Rust worker parses; tests pin the exact strings.

use chrono::{DateTime, Utc};
use oxo_tasks::{ClaimedTask, FailOutcome, JobCreated, JobStatus, Throughput};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Response to submitting a specification.
#[derive(Debug, Serialize)]
pub struct JobCreatedBody {
    pub job_id: Uuid,
    pub created: bool,
    pub total_tasks: u32,
}

impl From<JobCreated> for JobCreatedBody {
    fn from(value: JobCreated) -> Self {
        Self {
            job_id: value.job_id.as_uuid(),
            created: value.created,
            total_tasks: value.total_tasks,
        }
    }
}

/// A job's completion gate, tagged by `state`.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobStatusBody {
    Complete,
    Failed {
        abandoned: u32,
    },
    InProgress {
        pending: u32,
        claimed: u32,
        succeeded: u32,
        abandoned: u32,
    },
}

impl From<JobStatus> for JobStatusBody {
    fn from(value: JobStatus) -> Self {
        match value {
            JobStatus::Complete => Self::Complete,
            JobStatus::Failed { abandoned } => Self::Failed { abandoned },
            JobStatus::InProgress {
                pending,
                claimed,
                succeeded,
                abandoned,
            } => Self::InProgress {
                pending,
                claimed,
                succeeded,
                abandoned,
            },
        }
    }
}

/// The throughput signal.
#[derive(Debug, Serialize)]
pub struct ThroughputBody {
    pub pending: u32,
    pub claimable_now: u32,
    pub claimed: u32,
    pub succeeded: u32,
    pub abandoned: u32,
}

impl From<Throughput> for ThroughputBody {
    fn from(value: Throughput) -> Self {
        Self {
            pending: value.pending,
            claimable_now: value.claimable_now,
            claimed: value.claimed,
            succeeded: value.succeeded,
            abandoned: value.abandoned,
        }
    }
}

/// Identity of a job found by region and revision.
#[derive(Debug, Serialize)]
pub struct FoundJobBody {
    pub job_id: Uuid,
}

/// A worker's claim request. `task_types` absent means any type; present
/// and empty means none.
#[derive(Debug, Deserialize)]
pub struct ClaimBody {
    pub worker: String,
    #[serde(default)]
    pub task_types: Option<Vec<String>>,
}

/// A claimed task as the worker sees it.
#[derive(Debug, Serialize)]
pub struct ClaimedTaskBody {
    pub task_id: Uuid,
    pub job_id: Uuid,
    pub lease_token: Uuid,
    pub tile: String,
    pub task_type: String,
    pub attempt: u32,
}

impl From<ClaimedTask> for ClaimedTaskBody {
    fn from(value: ClaimedTask) -> Self {
        Self {
            task_id: value.task_id.as_uuid(),
            job_id: value.job_id.as_uuid(),
            lease_token: value.lease.as_uuid(),
            tile: value.tile.to_string(),
            task_type: value.task_type.as_str().to_string(),
            attempt: value.attempt,
        }
    }
}

/// Body of heartbeat and complete calls.
#[derive(Debug, Deserialize)]
pub struct LeaseBody {
    pub lease_token: Uuid,
}

/// Body of a fail call.
#[derive(Debug, Deserialize)]
pub struct FailBody {
    pub lease_token: Uuid,
    pub reason: String,
}

/// What happened to a failed task, tagged by `outcome`.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FailOutcomeBody {
    Requeued {
        claimable_at: DateTime<Utc>,
        attempts_remaining: u32,
    },
    Abandoned,
}

impl From<FailOutcome> for FailOutcomeBody {
    fn from(value: FailOutcome) -> Self {
        match value {
            FailOutcome::Requeued {
                claimable_at,
                attempts_remaining,
            } => Self::Requeued {
                claimable_at,
                attempts_remaining,
            },
            FailOutcome::Abandoned => Self::Abandoned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxo_spec::TileId;
    use oxo_tasks::{JobId, LeaseToken, TaskId, TaskType};

    #[test]
    fn job_status_serializes_tagged_by_state() {
        let complete = serde_json::to_value(JobStatusBody::from(JobStatus::Complete)).unwrap();
        assert_eq!(complete, serde_json::json!({"state": "complete"}));

        let failed =
            serde_json::to_value(JobStatusBody::from(JobStatus::Failed { abandoned: 2 })).unwrap();
        assert_eq!(
            failed,
            serde_json::json!({"state": "failed", "abandoned": 2})
        );

        let in_progress = serde_json::to_value(JobStatusBody::from(JobStatus::InProgress {
            pending: 1,
            claimed: 2,
            succeeded: 3,
            abandoned: 0,
        }))
        .unwrap();
        assert_eq!(
            in_progress,
            serde_json::json!({"state": "in_progress", "pending": 1, "claimed": 2, "succeeded": 3, "abandoned": 0})
        );
    }

    #[test]
    fn a_claimed_task_serializes_its_tile_and_type_canonically() {
        let claimed = ClaimedTask {
            task_id: TaskId::generate(),
            job_id: JobId::generate(),
            lease: LeaseToken::generate(),
            tile: TileId::new(50, -2).expect("in range"),
            task_type: TaskType::Overlay,
            attempt: 1,
        };
        let body = serde_json::to_value(ClaimedTaskBody::from(claimed)).unwrap();
        assert_eq!(body["tile"], "+50-002");
        assert_eq!(body["task_type"], "overlay");
        assert_eq!(body["attempt"], 1);
    }

    #[test]
    fn a_fail_outcome_serializes_tagged_by_outcome() {
        let abandoned =
            serde_json::to_value(FailOutcomeBody::from(FailOutcome::Abandoned)).unwrap();
        assert_eq!(abandoned, serde_json::json!({"outcome": "abandoned"}));
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-02T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let requeued = serde_json::to_value(FailOutcomeBody::from(FailOutcome::Requeued {
            claimable_at: at,
            attempts_remaining: 2,
        }))
        .unwrap();
        assert_eq!(requeued["outcome"], "requeued");
        assert_eq!(requeued["attempts_remaining"], 2);
        assert!(requeued["claimable_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-10-02T12:00:00"));
    }

    #[test]
    fn claim_bodies_distinguish_absent_from_empty_task_types() {
        let any: ClaimBody = serde_json::from_str(r#"{"worker": "w1"}"#).unwrap();
        assert_eq!(any.task_types, None);
        let none: ClaimBody =
            serde_json::from_str(r#"{"worker": "w1", "task_types": []}"#).unwrap();
        assert_eq!(none.task_types, Some(vec![]));
    }

    #[test]
    fn the_remaining_bodies_round_trip_their_contract_shapes() {
        let id = uuid::Uuid::nil();
        let created = serde_json::to_value(JobCreatedBody::from(JobCreated {
            job_id: JobId::from_uuid(id),
            created: true,
            total_tasks: 4,
        }))
        .unwrap();
        assert_eq!(
            created,
            serde_json::json!({"job_id": id, "created": true, "total_tasks": 4})
        );
        let through = serde_json::to_value(ThroughputBody::from(Throughput {
            pending: 1,
            claimable_now: 2,
            claimed: 3,
            succeeded: 4,
            abandoned: 5,
        }))
        .unwrap();
        assert_eq!(
            through,
            serde_json::json!({"pending": 1, "claimable_now": 2, "claimed": 3, "succeeded": 4, "abandoned": 5})
        );
        let lease: LeaseBody =
            serde_json::from_value(serde_json::json!({"lease_token": id})).unwrap();
        assert_eq!(lease.lease_token, id);
        let fail: FailBody =
            serde_json::from_value(serde_json::json!({"lease_token": id, "reason": "boom"}))
                .unwrap();
        assert_eq!(fail.reason, "boom");
        assert_eq!(
            serde_json::to_value(FoundJobBody { job_id: id }).unwrap(),
            serde_json::json!({"job_id": id})
        );
    }
}
