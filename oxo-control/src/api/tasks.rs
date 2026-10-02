use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oxo_tasks::{ClaimRequest, FailRequest, Lease, LeaseToken, TaskId, TaskType};
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::wire::{ClaimBody, ClaimedTaskBody, FailBody, FailOutcomeBody, LeaseBody};
use crate::api::AppState;

/// POST /api/v1/claims — 200 with a task, or 204 when nothing is
/// claimable, which is the normal idle state of a pull system.
pub(crate) async fn claim(
    State(state): State<AppState>,
    Json(body): Json<ClaimBody>,
) -> Result<Response, ApiError> {
    let task_types = match body.task_types {
        None => None,
        Some(names) => Some(
            names
                .into_iter()
                .map(|name| TaskType::from_str_exact(&name).ok_or(ApiError::UnknownTaskType(name)))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let claimed = state
        .store
        .claim(ClaimRequest {
            worker: body.worker,
            task_types,
        })
        .await?;
    Ok(match claimed {
        Some(task) => (StatusCode::OK, Json(ClaimedTaskBody::from(task))).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

fn lease(task_id: Uuid, token: Uuid) -> Lease {
    Lease {
        task_id: TaskId::from_uuid(task_id),
        token: LeaseToken::from_uuid(token),
    }
}

/// POST /api/v1/tasks/{task_id}/heartbeat
pub(crate) async fn heartbeat(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<LeaseBody>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .heartbeat(lease(task_id, body.lease_token))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/tasks/{task_id}/complete
pub(crate) async fn complete(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<LeaseBody>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .complete(lease(task_id, body.lease_token))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/tasks/{task_id}/fail
pub(crate) async fn fail(
    State(state): State<AppState>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<FailBody>,
) -> Result<Json<FailOutcomeBody>, ApiError> {
    let outcome = state
        .store
        .fail(FailRequest {
            lease: lease(task_id, body.lease_token),
            reason: body.reason,
        })
        .await?;
    Ok(Json(FailOutcomeBody::from(outcome)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;
    use oxo_tasks::{ReapRequest, TaskStore, TimeoutSeconds};
    use serde_json::json;

    use crate::api::test_support::{app, harness, post_json, post_toml, SPEC};

    /// One tile, no overlays: exactly one task, so reclaims are unambiguous.
    fn single_task_spec() -> String {
        let spec = SPEC
            .replace(
                r#"tiles = ["+50-002", "+51-002"]"#,
                r#"tiles = ["+50-002"]"#,
            )
            .replace("include_overlays = true", "include_overlays = false");
        assert!(spec.contains("include_overlays = false"));
        spec
    }

    #[tokio::test]
    async fn a_claim_hands_out_a_task_with_a_lease() {
        let app = app();
        post_toml(&app, "/api/v1/jobs", SPEC).await;
        let (status, body) = post_json(&app, "/api/v1/claims", json!({"worker": "w1"})).await;
        assert_eq!(status, StatusCode::OK);
        for field in ["task_id", "job_id", "lease_token"] {
            body[field]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap_or_else(|_| panic!("{field} is not a uuid: {body}"));
        }
        assert!(["+50-002", "+51-002"].contains(&body["tile"].as_str().unwrap()));
        assert!(["ortho", "overlay"].contains(&body["task_type"].as_str().unwrap()));
        assert_eq!(body["attempt"], 1);
    }

    #[tokio::test]
    async fn an_empty_queue_claims_nothing_with_204() {
        let app = app();
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/claims")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"worker": "w1"}).to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn a_type_filter_restricts_what_a_worker_receives() {
        let app = app();
        post_toml(&app, "/api/v1/jobs", SPEC).await;

        for _ in 0..2 {
            let (status, body) = post_json(
                &app,
                "/api/v1/claims",
                json!({"worker": "w1", "task_types": ["overlay"]}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["task_type"], "overlay");
        }
        // Only ortho tasks remain, so an overlay-only worker gets nothing.
        let (status, _) = post_json(
            &app,
            "/api/v1/claims",
            json!({"worker": "w1", "task_types": ["overlay"]}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // No capacity means no work, even though ortho tasks are pending.
        let (status, _) = post_json(
            &app,
            "/api/v1/claims",
            json!({"worker": "w1", "task_types": []}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, body) = post_json(
            &app,
            "/api/v1/claims",
            json!({"worker": "w1", "task_types": ["mesh"]}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "unknown_task_type");
    }

    #[tokio::test]
    async fn the_full_report_cycle_heartbeat_complete() {
        let app = app();
        post_toml(&app, "/api/v1/jobs", SPEC).await;
        let (_, claimed) = post_json(&app, "/api/v1/claims", json!({"worker": "w1"})).await;
        let task = claimed["task_id"].as_str().unwrap();
        let lease = claimed["lease_token"].as_str().unwrap();

        let (status, _) = post_json(
            &app,
            &format!("/api/v1/tasks/{task}/heartbeat"),
            json!({"lease_token": lease}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = post_json(
            &app,
            &format!("/api/v1/tasks/{task}/complete"),
            json!({"lease_token": lease}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, body) = post_json(
            &app,
            &format!("/api/v1/tasks/{task}/complete"),
            json!({"lease_token": lease}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_claimed");
    }

    #[tokio::test]
    async fn a_failure_reports_its_outcome() {
        let h = harness();
        post_toml(&h.app, "/api/v1/jobs", &single_task_spec()).await;

        for expected_remaining in [2, 1] {
            let (status, claimed) =
                post_json(&h.app, "/api/v1/claims", json!({"worker": "w1"})).await;
            assert_eq!(status, StatusCode::OK);
            let task = claimed["task_id"].as_str().unwrap();
            let (status, body) = post_json(
                &h.app,
                &format!("/api/v1/tasks/{task}/fail"),
                json!({"lease_token": claimed["lease_token"], "reason": "Crash!"}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["outcome"], "requeued");
            assert_eq!(body["attempts_remaining"], expected_remaining);
            assert!(body["claimable_at"].is_string());
            // Backoff: nothing is claimable until it elapses.
            let (status, _) = post_json(&h.app, "/api/v1/claims", json!({"worker": "w1"})).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
            h.clock.advance(Duration::from_secs(3600));
        }

        let (status, claimed) = post_json(&h.app, "/api/v1/claims", json!({"worker": "w1"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(claimed["attempt"], 3);
        let task = claimed["task_id"].as_str().unwrap();
        let (status, body) = post_json(
            &h.app,
            &format!("/api/v1/tasks/{task}/fail"),
            json!({"lease_token": claimed["lease_token"], "reason": "Crash!"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"outcome": "abandoned"}));
    }

    #[tokio::test]
    async fn a_stale_lease_is_409_with_lease_lost_or_not_claimed() {
        let h = harness();
        post_toml(&h.app, "/api/v1/jobs", &single_task_spec()).await;
        let (_, claimed) = post_json(&h.app, "/api/v1/claims", json!({"worker": "w1"})).await;
        let task = claimed["task_id"].as_str().unwrap().to_string();
        let old_lease = claimed["lease_token"].as_str().unwrap().to_string();

        // w1 stops heartbeating; the reaper (driven directly) reclaims it.
        h.clock.advance(Duration::from_secs(600));
        let reaped = h
            .store
            .reap_expired(ReapRequest {
                heartbeat_timeout: TimeoutSeconds::new(30).unwrap(),
                max_task_duration: TimeoutSeconds::new(86_400).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!((reaped.requeued, reaped.abandoned), (1, 0));

        // Pending again, nobody has re-claimed it: not_claimed.
        let (status, body) = post_json(
            &h.app,
            &format!("/api/v1/tasks/{task}/heartbeat"),
            json!({"lease_token": old_lease}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "not_claimed");

        // Another worker claims it (after any backoff): lease_lost.
        h.clock.advance(Duration::from_secs(3600));
        let (status, reclaimed) =
            post_json(&h.app, "/api/v1/claims", json!({"worker": "w2"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reclaimed["task_id"], task.as_str());
        assert_ne!(reclaimed["lease_token"], old_lease.as_str());

        let (status, body) = post_json(
            &h.app,
            &format!("/api/v1/tasks/{task}/heartbeat"),
            json!({"lease_token": old_lease}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "lease_lost");
    }

    #[tokio::test]
    async fn reporting_on_an_unknown_task_is_404() {
        let app = app();
        let task = uuid::Uuid::new_v4();
        let lease = uuid::Uuid::new_v4();
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/tasks/{task}/heartbeat"),
            json!({"lease_token": lease}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "unknown_task");
    }
}
