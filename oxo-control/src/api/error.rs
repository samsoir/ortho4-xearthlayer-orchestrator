//! The one place a fault becomes a status code.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oxo_tasks::TaskStoreError;
use serde::Serialize;

/// The one place a fault becomes a status code. Handlers return
/// `Result<_, ApiError>` and never choose their own mapping.
#[derive(Debug)]
pub enum ApiError {
    Store(TaskStoreError),
    /// The submitted specification failed to parse or validate; the
    /// message is the full all-faults report, exactly what the CLI prints.
    InvalidSpec(String),
    UnknownTaskType(String),
    NoSuchJob {
        region_code: String,
        revision: u32,
    },
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: &'static str,
    pub message: String,
}

impl From<TaskStoreError> for ApiError {
    fn from(error: TaskStoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Store(error) => {
                let (status, code) = match &error {
                    TaskStoreError::LeaseLost { .. } => (StatusCode::CONFLICT, "lease_lost"),
                    TaskStoreError::NotClaimed { .. } => (StatusCode::CONFLICT, "not_claimed"),
                    TaskStoreError::UnknownJob { .. } => (StatusCode::NOT_FOUND, "unknown_job"),
                    TaskStoreError::UnknownTask { .. } => (StatusCode::NOT_FOUND, "unknown_task"),
                    TaskStoreError::JobConflict { .. } => (StatusCode::CONFLICT, "job_conflict"),
                    TaskStoreError::DuplicateTask { .. } => {
                        (StatusCode::UNPROCESSABLE_ENTITY, "duplicate_task")
                    }
                    TaskStoreError::EmptyJob { .. } => {
                        (StatusCode::UNPROCESSABLE_ENTITY, "empty_job")
                    }
                    TaskStoreError::Adapter(_) => (StatusCode::SERVICE_UNAVAILABLE, "adapter"),
                    // The port is #[non_exhaustive]; an unknown variant is
                    // an adapter-shaped surprise, not a client fault.
                    _ => (StatusCode::SERVICE_UNAVAILABLE, "adapter"),
                };
                (status, code, error.to_string())
            }
            Self::InvalidSpec(report) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_spec", report),
            Self::UnknownTaskType(name) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_task_type",
                format!("unknown task type {name:?}; expected \"ortho\" or \"overlay\""),
            ),
            Self::NoSuchJob {
                region_code,
                revision,
            } => (
                StatusCode::NOT_FOUND,
                "unknown_job",
                format!("no job for region {region_code} revision {revision}"),
            ),
        };
        (
            status,
            Json(ErrorBody {
                error: code,
                message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use oxo_spec::TileId;
    use oxo_tasks::{JobId, TaskId, TaskType};

    use super::*;

    async fn status_and_body(error: ApiError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn status_and_code(error: ApiError) -> (StatusCode, String) {
        let (status, body) = status_and_body(error).await;
        (status, body["error"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn every_store_error_maps_per_the_design_table() {
        let task_id = TaskId::generate();
        let job_id = JobId::generate();
        let tile = TileId::new(50, -2).expect("in range");
        let cases = vec![
            (
                TaskStoreError::LeaseLost { task_id },
                StatusCode::CONFLICT,
                "lease_lost",
            ),
            (
                TaskStoreError::NotClaimed { task_id },
                StatusCode::CONFLICT,
                "not_claimed",
            ),
            (
                TaskStoreError::UnknownJob { job_id },
                StatusCode::NOT_FOUND,
                "unknown_job",
            ),
            (
                TaskStoreError::UnknownTask { task_id },
                StatusCode::NOT_FOUND,
                "unknown_task",
            ),
            (
                TaskStoreError::JobConflict {
                    region_code: "NA".into(),
                    revision: 1,
                },
                StatusCode::CONFLICT,
                "job_conflict",
            ),
            (
                TaskStoreError::DuplicateTask {
                    tile,
                    task_type: TaskType::Ortho,
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "duplicate_task",
            ),
            (
                TaskStoreError::EmptyJob {
                    region_code: "NA".into(),
                    revision: 1,
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_job",
            ),
            (
                TaskStoreError::Adapter("connection reset".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "adapter",
            ),
        ];
        for (error, want_status, want_code) in cases {
            let label = format!("{error:?}");
            let (status, code) = status_and_code(ApiError::from(error)).await;
            assert_eq!(status, want_status, "status for {label}");
            assert_eq!(code, want_code, "code for {label}");
        }
    }

    #[tokio::test]
    async fn api_level_refusals_map_to_their_own_codes() {
        let report = "2 faults:\n- a\n- b".to_string();
        let (status, body) = status_and_body(ApiError::InvalidSpec(report.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "invalid_spec");
        assert_eq!(body["message"], report.as_str());

        let (status, code) = status_and_code(ApiError::UnknownTaskType("mesh".into())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code, "unknown_task_type");

        let (status, code) = status_and_code(ApiError::NoSuchJob {
            region_code: "NA".into(),
            revision: 3,
        })
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "unknown_job");
    }
}
