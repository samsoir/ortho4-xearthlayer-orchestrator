//! Specification → task set. One ortho task per tile; one overlay task
//! per tile additionally when the specification asks for overlays. Up to
//! 2N tasks from N tiles. `include_overlays = false` yields N ortho tasks
//! and is a first-class choice, not a degraded mode.

use oxo_spec::RegionSpec;
use oxo_tasks::{BackoffSeconds, CreateJob, InvalidQuantity, MaxAttempts, TaskSpec, TaskType};
use thiserror::Error;

/// Why a specification could not be planned.
///
/// Both cases are unreachable from a specification that passed
/// `oxo-spec` validation — it already refuses `max_attempts = 0` — but
/// `plan`'s input type cannot prove that, so the refusal is propagated
/// as a value, never unwrapped.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("failure policy max_attempts: {0}")]
    MaxAttempts(InvalidQuantity),
    #[error("failure policy backoff_seconds: {0}")]
    Backoff(InvalidQuantity),
}

/// Atomize a validated region specification into a job registration.
///
/// Deterministic: tiles in `BTreeSet` order, each tile's ortho task
/// before its overlay task, so re-planning an unchanged specification
/// reproduces the task set element for element.
pub fn plan(spec: &RegionSpec) -> Result<CreateJob, PlanError> {
    let max_attempts =
        MaxAttempts::new(spec.failure_policy.max_attempts).map_err(PlanError::MaxAttempts)?;
    let backoff =
        BackoffSeconds::new(spec.failure_policy.backoff_seconds).map_err(PlanError::Backoff)?;

    let per_tile = if spec.parameters.include_overlays {
        2
    } else {
        1
    };
    let mut tasks = Vec::with_capacity(spec.tiles.len() * per_tile);
    for &tile in &spec.tiles {
        tasks.push(TaskSpec {
            tile,
            task_type: TaskType::Ortho,
        });
        if spec.parameters.include_overlays {
            tasks.push(TaskSpec {
                tile,
                task_type: TaskType::Overlay,
            });
        }
    }

    Ok(CreateJob {
        region_code: spec.metadata.region_code.clone(),
        revision: spec.metadata.revision,
        max_attempts,
        backoff,
        tasks,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use oxo_spec::{
        FailurePolicy, Metadata, ProductionParameters, RegionSpec, TargetLocation, TileId,
    };
    use oxo_tasks::TaskType;

    use super::*;

    fn spec(tiles: &[(i8, i16)], include_overlays: bool) -> RegionSpec {
        RegionSpec {
            tiles: tiles
                .iter()
                .map(|&(lat, lon)| TileId::new(lat, lon).expect("in range"))
                .collect::<BTreeSet<_>>(),
            metadata: Metadata {
                name: "North America".to_string(),
                region_code: "NA".to_string(),
                revision: 1,
            },
            parameters: ProductionParameters {
                provider: "BI".to_string(),
                zoom: 16,
                include_overlays,
                raw: BTreeMap::new(),
            },
            target: TargetLocation {
                root: "/srv/oxo/artifacts/NA".into(),
            },
            failure_policy: FailurePolicy {
                max_attempts: 3,
                backoff_seconds: 60,
                alert_destinations: Vec::new(),
            },
        }
    }

    #[test]
    fn without_overlays_each_tile_becomes_one_ortho_task() {
        let job = plan(&spec(&[(50, -2), (51, -2)], false)).expect("plan");
        assert_eq!(job.tasks.len(), 2);
        assert!(job.tasks.iter().all(|t| t.task_type == TaskType::Ortho));
    }

    #[test]
    fn with_overlays_each_tile_becomes_an_ortho_and_an_overlay_task() {
        let job = plan(&spec(&[(50, -2), (51, -2)], true)).expect("plan");
        let types: Vec<_> = job.tasks.iter().map(|t| t.task_type).collect();
        assert_eq!(
            types,
            vec![
                TaskType::Ortho,
                TaskType::Overlay,
                TaskType::Ortho,
                TaskType::Overlay
            ]
        );
        assert_eq!(job.tasks[0].tile, job.tasks[1].tile);
        assert_ne!(job.tasks[0].tile, job.tasks[2].tile);
    }

    #[test]
    fn identity_and_policy_are_copied_from_the_specification() {
        let job = plan(&spec(&[(50, -2)], false)).expect("plan");
        assert_eq!(job.region_code, "NA");
        assert_eq!(job.revision, 1);
        assert_eq!(job.max_attempts.get(), 3);
        assert_eq!(job.backoff.get(), 60);
    }

    #[test]
    fn planning_is_deterministic_for_an_unchanged_specification() {
        // create_job's idempotency compares task sets; a planner that
        // reordered between runs would still resume (comparison is
        // order-insensitive) but reproducibility is the promise here.
        let first = plan(&spec(&[(51, -2), (50, -2), (50, -3)], true)).expect("plan");
        let second = plan(&spec(&[(50, -3), (51, -2), (50, -2)], true)).expect("plan");
        assert_eq!(first, second);
    }

    #[test]
    fn an_unvalidated_zero_max_attempts_is_refused_not_unwrapped() {
        // oxo-spec validation already refuses this, but plan's input type
        // cannot prove its argument was validated, so the error must be a
        // value, never a panic.
        let mut bad = spec(&[(50, -2)], false);
        bad.failure_policy.max_attempts = 0;
        let error = plan(&bad).expect_err("refuse");
        assert!(matches!(
            error,
            PlanError::MaxAttempts(InvalidQuantity::Zero)
        ));
        assert!(error.to_string().contains("max_attempts"), "{error}");
    }

    #[test]
    fn an_unrepresentable_backoff_is_refused_not_unwrapped() {
        let mut bad = spec(&[(50, -2)], false);
        bad.failure_policy.backoff_seconds = u64::MAX;
        let error = plan(&bad).expect_err("refuse");
        assert!(matches!(
            error,
            PlanError::Backoff(InvalidQuantity::TooManySeconds(_))
        ));
    }
}
