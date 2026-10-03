//! A reference client of the control plane's HTTP contract. The DTOs are
//! this crate's own: the wire shape, not the server's types, is the
//! contract.

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// A task as claimed, with the region's production intent in `config`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedTask {
    pub task_id: Uuid,
    pub job_id: Uuid,
    pub lease_token: Uuid,
    pub tile: String,
    pub task_type: String,
    pub attempt: u32,
    pub config: serde_json::Value,
}

/// What the control plane did with a reported failure.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FailOutcome {
    Requeued {
        claimable_at: String,
        attempts_remaining: u32,
    },
    Abandoned,
}

/// The control plane's error body.
#[derive(Debug, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
}

/// The error split the worker loop acts on.
#[derive(Debug, thiserror::Error)]
pub enum ApiFailure {
    /// Any 409: the lease is no longer ours. Stop the task. The body's
    /// error code is logged, never branched on.
    #[error("lease gone: {0}")]
    LeaseGone(String),
    /// 503 or a transport error: wait and try again.
    #[error("retryable: {0}")]
    Retryable(String),
    /// Anything else: the contract is broken; exit.
    #[error("fatal: HTTP {0}: {1}")]
    Fatal(u16, String),
}

/// Maps a non-success response onto the error split.
fn classify(status: u16, body: String) -> ApiFailure {
    match status {
        409 => {
            let detail = match serde_json::from_str::<ErrorBody>(&body) {
                Ok(parsed) => {
                    tracing::warn!(code = %parsed.error, "409 from the control plane");
                    format!("{}: {}", parsed.error, parsed.message)
                }
                Err(_) => body,
            };
            ApiFailure::LeaseGone(detail)
        }
        503 => ApiFailure::Retryable(format!("HTTP 503: {body}")),
        _ => ApiFailure::Fatal(status, body),
    }
}

/// Per-request timeout, kept below the heartbeat interval floor: a hung
/// report or heartbeat must not leave the child unsupervised. A timeout
/// surfaces as `Retryable`.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ControlPlane {
    base: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct LeaseBody {
    lease_token: Uuid,
}

impl ControlPlane {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("a client with a timeout builds"),
        }
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<(u16, String), ApiFailure> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiFailure::Retryable(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| ApiFailure::Retryable(e.to_string()))?;
        if (200..300).contains(&status) {
            Ok((status, text))
        } else {
            Err(classify(status, text))
        }
    }

    /// Claim a task of one of the given types (empty means none; the
    /// caller states what it can run). `Ok(None)` is the idle state.
    pub async fn claim(
        &self,
        worker: &str,
        task_types: &[&str],
    ) -> Result<Option<ClaimedTask>, ApiFailure> {
        let (status, text) = self
            .post(
                "/api/v1/claims",
                json!({"worker": worker, "task_types": task_types}),
            )
            .await?;
        if status == 204 {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| ApiFailure::Fatal(status, format!("unparseable claim: {e}: {text}")))
    }

    pub async fn heartbeat(&self, task_id: Uuid, lease: Uuid) -> Result<(), ApiFailure> {
        self.lease_call(task_id, "heartbeat", lease).await
    }

    pub async fn complete(&self, task_id: Uuid, lease: Uuid) -> Result<(), ApiFailure> {
        self.lease_call(task_id, "complete", lease).await
    }

    async fn lease_call(&self, task_id: Uuid, verb: &str, lease: Uuid) -> Result<(), ApiFailure> {
        let body = serde_json::to_value(LeaseBody { lease_token: lease }).expect("serializable");
        self.post(&format!("/api/v1/tasks/{task_id}/{verb}"), body)
            .await
            .map(|_| ())
    }

    pub async fn fail(
        &self,
        task_id: Uuid,
        lease: Uuid,
        reason: &str,
    ) -> Result<FailOutcome, ApiFailure> {
        let (status, text) = self
            .post(
                &format!("/api/v1/tasks/{task_id}/fail"),
                json!({"lease_token": lease, "reason": reason}),
            )
            .await?;
        serde_json::from_str(&text)
            .map_err(|e| ApiFailure::Fatal(status, format!("unparseable outcome: {e}: {text}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_409_is_lease_gone_whatever_its_code() {
        for code in ["lease_lost", "not_claimed", "job_conflict", "garbage"] {
            let body = format!(r#"{{"error":"{code}","message":"m"}}"#);
            assert!(matches!(classify(409, body), ApiFailure::LeaseGone(_)));
        }
        assert!(matches!(
            classify(409, "not json".into()),
            ApiFailure::LeaseGone(_)
        ));
    }

    #[test]
    fn a_503_is_retryable_and_everything_else_fatal() {
        assert!(matches!(
            classify(503, String::new()),
            ApiFailure::Retryable(_)
        ));
        for status in [400, 404, 422, 500] {
            assert!(matches!(
                classify(status, "b".into()),
                ApiFailure::Fatal(s, _) if s == status
            ));
        }
    }
}
