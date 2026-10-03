//! The client against the real router, served in-process on an ephemeral
//! port. No mock of the contract: the router is the contract.

use std::sync::Arc;
use std::time::Duration;

use oxo_control::api::router;
use oxo_tasks::{InMemoryTaskStore, ReapRequest, TaskStore, TestClock, TimeoutSeconds};
use oxo_worker::api::{ApiFailure, ControlPlane, FailOutcome};
use tokio::net::TcpListener;

/// The canonical two-tile specification (copied from the control plane's
/// own test support, which is not exported).
const SPEC: &str = r#"
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

struct Harness {
    client: ControlPlane,
    store: Arc<InMemoryTaskStore>,
    clock: Arc<TestClock>,
    base: String,
    server: tokio::task::JoinHandle<()>,
}

async fn harness(spec: &str) -> Harness {
    let clock = Arc::new(TestClock::new(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    ));
    let store = Arc::new(InMemoryTaskStore::new(clock.clone()));
    let app = router(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/jobs"))
        .body(spec.to_string())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    Harness {
        client: ControlPlane::new(&base),
        store,
        clock,
        base,
        server,
    }
}

fn single_task_spec() -> String {
    SPEC.replace(
        r#"tiles = ["+50-002", "+51-002"]"#,
        r#"tiles = ["+50-002"]"#,
    )
    .replace("include_overlays = true", "include_overlays = false")
}

#[tokio::test]
async fn a_claim_returns_the_task_with_its_config_intact() {
    let h = harness(SPEC).await;
    let task = h
        .client
        .claim("w1", &["ortho"])
        .await
        .expect("claim")
        .expect("a task");
    // Same-instant tasks are claimed in UUID tie-break order, so which
    // tile arrives is not specified; the job-wide config is.
    assert!(
        ["+50-002", "+51-002"].contains(&task.tile.as_str()),
        "{}",
        task.tile
    );
    assert!(["ortho", "overlay"].contains(&task.task_type.as_str()));
    assert_eq!(task.attempt, 1);
    assert_eq!(
        task.config,
        serde_json::json!({"v": 1, "provider": "BI", "zoom": 16, "raw": {}, "target_root": "/srv/oxo/artifacts/NA"})
    );
}

#[tokio::test]
async fn nothing_claimable_is_ok_none() {
    let h = harness(&single_task_spec()).await;
    assert!(h.client.claim("w1", &["ortho"]).await.unwrap().is_some());
    assert!(h
        .client
        .claim("w2", &["ortho"])
        .await
        .expect("claim")
        .is_none());
}

#[tokio::test]
async fn heartbeat_then_complete_succeeds() {
    let h = harness(&single_task_spec()).await;
    let task = h.client.claim("w1", &["ortho"]).await.unwrap().unwrap();
    h.client
        .heartbeat(task.task_id, task.lease_token)
        .await
        .expect("heartbeat");
    h.client
        .complete(task.task_id, task.lease_token)
        .await
        .expect("complete");
    assert!(h.client.claim("w1", &["ortho"]).await.unwrap().is_none());
}

#[tokio::test]
async fn fail_reports_the_outcome() {
    let h = harness(&single_task_spec()).await;
    let task = h.client.claim("w1", &["ortho"]).await.unwrap().unwrap();
    let outcome = h
        .client
        .fail(task.task_id, task.lease_token, "Crash!")
        .await
        .expect("fail");
    assert!(
        matches!(
            outcome,
            FailOutcome::Requeued {
                attempts_remaining: 2,
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_lost_lease_is_lease_gone() {
    let h = harness(&single_task_spec()).await;
    let old = h.client.claim("w1", &["ortho"]).await.unwrap().unwrap();

    // w1 stops heartbeating; the reaper (driven directly) reclaims it,
    // and a second worker takes it after the backoff.
    h.clock.advance(Duration::from_secs(600));
    h.store
        .reap_expired(ReapRequest {
            heartbeat_timeout: TimeoutSeconds::new(30).unwrap(),
            max_task_duration: TimeoutSeconds::new(86_400).unwrap(),
        })
        .await
        .unwrap();
    h.clock.advance(Duration::from_secs(3600));
    let fresh = h.client.claim("w2", &["ortho"]).await.unwrap().unwrap();
    assert_eq!(fresh.task_id, old.task_id);

    for result in [
        h.client.heartbeat(old.task_id, old.lease_token).await,
        h.client.complete(old.task_id, old.lease_token).await,
    ] {
        assert!(
            matches!(result, Err(ApiFailure::LeaseGone(_))),
            "{result:?}"
        );
    }
    let failed = h.client.fail(old.task_id, old.lease_token, "x").await;
    assert!(
        matches!(failed, Err(ApiFailure::LeaseGone(_))),
        "{failed:?}"
    );
}

#[tokio::test]
async fn an_unknown_task_is_fatal_with_status_and_body() {
    let h = harness(&single_task_spec()).await;
    let error = h
        .client
        .heartbeat(uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
        .await
        .expect_err("404");
    match error {
        ApiFailure::Fatal(status, body) => {
            assert_eq!(status, 404);
            assert!(body.contains("unknown_task"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_task_type_is_fatal() {
    let h = harness(&single_task_spec()).await;
    let error = h.client.claim("w1", &["bogus"]).await.expect_err("422");
    assert!(matches!(error, ApiFailure::Fatal(422, _)), "{error:?}");
}

#[tokio::test]
async fn a_stopped_server_is_retryable() {
    let h = harness(&single_task_spec()).await;
    h.server.abort();
    let _ = h.server.await;
    let client = ControlPlane::new(&h.base);
    let error = client.claim("w1", &[]).await.expect_err("refused");
    assert!(matches!(error, ApiFailure::Retryable(_)), "{error:?}");
}
