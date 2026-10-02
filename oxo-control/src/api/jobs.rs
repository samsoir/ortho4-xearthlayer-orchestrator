use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use oxo_spec::RegionSpec;
use oxo_tasks::request::FindJob;
use oxo_tasks::JobId;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::wire::{FoundJobBody, JobCreatedBody, JobStatusBody, ThroughputBody};
use crate::api::AppState;
use crate::planner;

/// POST /api/v1/jobs — body is the canonical specification TOML.
pub(crate) async fn submit(
    State(state): State<AppState>,
    body: String,
) -> Result<(StatusCode, Json<JobCreatedBody>), ApiError> {
    let spec = RegionSpec::from_toml(&body).map_err(|e| ApiError::InvalidSpec(e.to_string()))?;
    let job = planner::plan(&spec).map_err(|e| ApiError::InvalidSpec(e.to_string()))?;
    let created = state.store.create_job(job).await?;
    let status = if created.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(JobCreatedBody::from(created))))
}

#[derive(Deserialize)]
pub(crate) struct FindQuery {
    region_code: String,
    revision: u32,
}

/// GET /api/v1/jobs?region_code=NA&revision=1
pub(crate) async fn find(
    State(state): State<AppState>,
    Query(query): Query<FindQuery>,
) -> Result<Json<FoundJobBody>, ApiError> {
    let found = state
        .store
        .find_job(FindJob {
            region_code: query.region_code.clone(),
            revision: query.revision,
        })
        .await?;
    match found {
        Some(job_id) => Ok(Json(FoundJobBody {
            job_id: job_id.as_uuid(),
        })),
        None => Err(ApiError::NoSuchJob {
            region_code: query.region_code,
            revision: query.revision,
        }),
    }
}

/// GET /api/v1/jobs/{job_id}
pub(crate) async fn status(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<JobStatusBody>, ApiError> {
    let status = state.store.job_status(JobId::from_uuid(job_id)).await?;
    Ok(Json(JobStatusBody::from(status)))
}

/// GET /api/v1/jobs/{job_id}/throughput
pub(crate) async fn throughput(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<ThroughputBody>, ApiError> {
    let snapshot = state.store.throughput(JobId::from_uuid(job_id)).await?;
    Ok(Json(ThroughputBody::from(snapshot)))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::api::test_support::{app, get, job_id, post_toml, SPEC};

    #[tokio::test]
    async fn submitting_a_specification_creates_the_planned_job() {
        let app = app();
        let (status, body) = post_toml(&app, "/api/v1/jobs", SPEC).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["created"], true);
        assert_eq!(body["total_tasks"], 4);
        let _ = job_id(&body);
    }

    #[tokio::test]
    async fn resubmitting_the_same_specification_resumes_not_duplicates() {
        let app = app();
        let (first_status, first) = post_toml(&app, "/api/v1/jobs", SPEC).await;
        assert_eq!(first_status, StatusCode::CREATED);
        let (second_status, second) = post_toml(&app, "/api/v1/jobs", SPEC).await;
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(second["created"], false);
        assert_eq!(job_id(&second), job_id(&first));
    }

    #[tokio::test]
    async fn a_faulty_specification_gets_the_whole_validation_report() {
        let app = app();
        let faulty = SPEC.replace(r#"tiles = ["+50-002", "+51-002"]"#, "tiles = []");
        assert_ne!(faulty, SPEC);
        let (status, body) = post_toml(&app, "/api/v1/jobs", &faulty).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "invalid_spec");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("tile set is empty"),
            "message was {}",
            body["message"]
        );
    }

    #[tokio::test]
    async fn unparseable_toml_is_refused_as_invalid_spec() {
        let app = app();
        let (status, body) = post_toml(&app, "/api/v1/jobs", "this is not toml").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "invalid_spec");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("could not parse specification"),
            "message was {}",
            body["message"]
        );
    }

    #[tokio::test]
    async fn a_job_is_findable_by_the_identity_the_operator_knows() {
        let app = app();
        let (_, created) = post_toml(&app, "/api/v1/jobs", SPEC).await;
        let (status, found) = get(&app, "/api/v1/jobs?region_code=NA&revision=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(job_id(&found), job_id(&created));

        let (status, missing) = get(&app, "/api/v1/jobs?region_code=NA&revision=2").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"], "unknown_job");
    }

    #[tokio::test]
    async fn status_and_throughput_answer_for_a_real_job_and_404_otherwise() {
        let app = app();
        let (_, created) = post_toml(&app, "/api/v1/jobs", SPEC).await;
        let id = job_id(&created);

        let (status, body) = get(&app, &format!("/api/v1/jobs/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "in_progress");
        assert_eq!(body["pending"], 4);

        let (status, body) = get(&app, &format!("/api/v1/jobs/{id}/throughput")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pending"], 4);
        assert_eq!(body["claimable_now"], 4);

        let other = uuid::Uuid::new_v4();
        let (status, body) = get(&app, &format!("/api/v1/jobs/{other}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "unknown_job");
        let (status, _) = get(&app, &format!("/api/v1/jobs/{other}/throughput")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn healthz_answers_200() {
        let app = app();
        let request = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
