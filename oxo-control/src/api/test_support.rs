//! Shared harness for the handler tests: a router over the in-memory
//! store, with the store and the test clock exposed for direct driving.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use oxo_tasks::{InMemoryTaskStore, TestClock};
use tower::ServiceExt;

use crate::api::router;

pub(crate) const SPEC: &str = r#"
tiles = ["+50-002", "+51-002"]

[metadata]
name = "North America"
region_code = "NA"
revision = 1

[parameters]
provider = "BI"
zoom = 16
include_overlays = true

[target]
root = "/srv/oxo/artifacts/NA"

[failure_policy]
max_attempts = 3
backoff_seconds = 60
"#;

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) store: Arc<InMemoryTaskStore>,
    pub(crate) clock: Arc<TestClock>,
}

pub(crate) fn harness() -> Harness {
    let clock = Arc::new(TestClock::new(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    ));
    let store = Arc::new(InMemoryTaskStore::new(clock.clone()));
    Harness {
        app: router(store.clone()),
        store,
        clock,
    }
}

pub(crate) fn app() -> Router {
    harness().app
}

pub(crate) async fn send(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

pub(crate) async fn post_toml(
    app: &Router,
    uri: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, request).await
}

pub(crate) async fn post_json(
    app: &Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, request).await
}

pub(crate) async fn get(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    send(app, request).await
}

pub(crate) fn job_id(body: &serde_json::Value) -> uuid::Uuid {
    body["job_id"].as_str().unwrap().parse().unwrap()
}
