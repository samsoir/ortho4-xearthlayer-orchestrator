//! The HTTP surface. Wire types are owned here, serialized with serde;
//! the port types in oxo-tasks stay serde-free, so the wire contract can
//! change without touching the port.

pub mod error;
pub mod wire;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use oxo_tasks::TaskStore;

mod jobs;
mod tasks;
#[cfg(test)]
mod test_support;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<dyn TaskStore>,
}

/// The whole HTTP surface over an injected store. The composition root
/// decides which adapter sits behind it; tests inject the in-memory one.
pub fn router(store: Arc<dyn TaskStore>) -> Router {
    Router::new()
        .route("/healthz", get(|| async {}))
        .route("/api/v1/jobs", post(jobs::submit).get(jobs::find))
        .route("/api/v1/jobs/{job_id}", get(jobs::status))
        .route("/api/v1/jobs/{job_id}/throughput", get(jobs::throughput))
        .route("/api/v1/claims", post(tasks::claim))
        .route("/api/v1/tasks/{task_id}/heartbeat", post(tasks::heartbeat))
        .route("/api/v1/tasks/{task_id}/complete", post(tasks::complete))
        .route("/api/v1/tasks/{task_id}/fail", post(tasks::fail))
        .with_state(AppState { store })
}
