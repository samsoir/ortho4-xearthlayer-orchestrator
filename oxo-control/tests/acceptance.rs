//! Acceptance scenarios for the control plane, driven through the real
//! router over the in-memory store. No network and no sleeps: requests go
//! through `tower::ServiceExt::oneshot`, and the clock is a fixed test clock.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use cucumber::{given, then, when, World};
use http_body_util::BodyExt;
use oxo_tasks::{InMemoryTaskStore, TestClock};
use serde_json::{json, Value};
use tower::ServiceExt;

#[derive(Debug, World)]
#[world(init = Self::new)]
struct ControlWorld {
    app: Router,
    tiles: Vec<String>,
    include_overlays: bool,
    max_attempts: u32,
    spec: String,
    /// Status and body of the most recent submission.
    submission: Option<(StatusCode, Value)>,
    /// Status and body of the first submission, kept when a second follows.
    first_submission: Option<(StatusCode, Value)>,
    /// Every `(tile, task_type)` pair handed out by a claim, with its count.
    claimed: BTreeMap<(String, String), usize>,
    /// The `outcome` of each failure report, in order.
    fail_outcomes: Vec<String>,
}

impl ControlWorld {
    fn new() -> Self {
        let clock = Arc::new(TestClock::new(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp"),
        ));
        let store = Arc::new(InMemoryTaskStore::new(clock));
        Self {
            app: oxo_control::api::router(store),
            tiles: Vec::new(),
            include_overlays: true,
            max_attempts: 3,
            spec: String::new(),
            submission: None,
            first_submission: None,
            claimed: BTreeMap::new(),
            fail_outcomes: Vec::new(),
        }
    }

    fn spec_toml(&self, region: &str) -> String {
        let tiles = self
            .tiles
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "tiles = [{tiles}]\n\
             \n\
             [metadata]\n\
             name = \"{region} region\"\n\
             region_code = \"{region}\"\n\
             revision = 1\n\
             \n\
             [parameters]\n\
             provider = \"BI\"\n\
             zoom = 16\n\
             include_overlays = {overlays}\n\
             \n\
             [target]\n\
             root = \"/srv/oxo/artifacts/{region}\"\n\
             \n\
             [failure_policy]\n\
             max_attempts = {attempts}\n\
             backoff_seconds = 0\n",
            overlays = self.include_overlays,
            attempts = self.max_attempts,
        )
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router is infallible");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("valid request");
        self.send(request).await
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("valid request");
        self.send(request).await
    }

    async fn submit_spec(&mut self) {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/jobs")
            .body(Body::from(self.spec.clone()))
            .expect("valid request");
        let response = self.send(request).await;
        self.submission = Some(response);
    }

    fn submitted(&self) -> &(StatusCode, Value) {
        self.submission
            .as_ref()
            .expect("a specification was submitted")
    }

    fn job_id(&self) -> String {
        self.submitted().1["job_id"]
            .as_str()
            .expect("submission carries a job_id")
            .to_string()
    }

    async fn claim(&mut self) -> (StatusCode, Value) {
        let response = self
            .post_json("/api/v1/claims", json!({"worker": "acceptance-worker"}))
            .await;
        if response.0 == StatusCode::OK {
            let key = (
                response.1["tile"].as_str().expect("tile").to_string(),
                response.1["task_type"]
                    .as_str()
                    .expect("task_type")
                    .to_string(),
            );
            *self.claimed.entry(key).or_insert(0) += 1;
        }
        response
    }
}

#[given("a control plane with an empty store")]
fn an_empty_control_plane(world: &mut ControlWorld) {
    *world = ControlWorld::new();
}

#[when(
    regex = r#"^the operator submits a specification for region "([A-Z]+)" with tiles "([^"]+)" and "([^"]+)" including overlays$"#
)]
async fn submits_two_tiles_with_overlays(
    world: &mut ControlWorld,
    region: String,
    first: String,
    second: String,
) {
    world.tiles = vec![first, second];
    world.include_overlays = true;
    world.spec = world.spec_toml(&region);
    world.submit_spec().await;
}

#[when(
    regex = r#"^the operator submits a specification for region "([A-Z]+)" with tiles "([^"]+)" including overlays disabled, two attempts and no backoff$"#
)]
async fn submits_one_tile_without_overlays(world: &mut ControlWorld, region: String, tile: String) {
    world.tiles = vec![tile];
    world.include_overlays = false;
    world.max_attempts = 2;
    world.spec = world.spec_toml(&region);
    world.submit_spec().await;
}

#[when("the operator submits the same specification again")]
async fn submits_again(world: &mut ControlWorld) {
    world.first_submission = world.submission.take();
    world.submit_spec().await;
}

#[then("the submission is accepted with one ortho and one overlay task per tile")]
fn accepted_with_both_task_types(world: &mut ControlWorld) {
    let (status, body) = world.submitted();
    assert_eq!(*status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["created"], json!(true));
    assert_eq!(body["total_tasks"], json!(world.tiles.len() * 2));
}

#[when("workers claim and complete every task")]
async fn workers_claim_and_complete(world: &mut ControlWorld) {
    // Bounded: the job holds `total_tasks` tasks, so one more claim than
    // that must already have answered 204 — otherwise the store is
    // handing out work it should not have.
    let total = world.submitted().1["total_tasks"]
        .as_u64()
        .expect("total_tasks") as usize;
    let mut drained = false;
    for _ in 0..=total {
        let (status, claimed) = world.claim().await;
        if status == StatusCode::NO_CONTENT {
            drained = true;
            break;
        }
        assert_eq!(status, StatusCode::OK, "claim body: {claimed}");
        let uri = format!(
            "/api/v1/tasks/{}/complete",
            claimed["task_id"].as_str().unwrap()
        );
        let (status, body) = world
            .post_json(&uri, json!({"lease_token": claimed["lease_token"]}))
            .await;
        assert!(status.is_success(), "complete answered {status}: {body}");
    }
    assert!(drained, "claims did not drain within {total} tasks");

    let mut expected = BTreeMap::new();
    for tile in &world.tiles {
        for task_type in ["ortho", "overlay"] {
            expected.insert((tile.clone(), task_type.to_string()), 1usize);
        }
    }
    assert_eq!(
        world.claimed, expected,
        "each (tile, task_type) must be claimed exactly once"
    );
}

#[then("the job reports complete")]
async fn job_reports_complete(world: &mut ControlWorld) {
    let uri = format!("/api/v1/jobs/{}", world.job_id());
    let (status, body) = world.get(&uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], json!("complete"), "status body: {body}");
}

#[when(
    regex = r#"^a worker claims the task and reports failure with reason "([^"]+)" until it is abandoned$"#
)]
async fn fails_until_abandoned(world: &mut ControlWorld, reason: String) {
    // Bounded by the attempt budget: the task must be abandoned on or
    // before the `max_attempts`-th failure.
    let budget = world.max_attempts as usize;
    for _ in 0..budget {
        let (status, claimed) = world.claim().await;
        assert_eq!(status, StatusCode::OK, "claim body: {claimed}");
        let uri = format!(
            "/api/v1/tasks/{}/fail",
            claimed["task_id"].as_str().unwrap()
        );
        let (status, body) = world
            .post_json(
                &uri,
                json!({"lease_token": claimed["lease_token"], "reason": reason}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "fail body: {body}");
        let outcome = body["outcome"].as_str().expect("outcome").to_string();
        let abandoned = outcome == "abandoned";
        world.fail_outcomes.push(outcome);
        if abandoned {
            break;
        }
    }
    assert_eq!(
        world.fail_outcomes.first().map(String::as_str),
        Some("requeued"),
        "the first failure leaves budget, so it requeues"
    );
    assert_eq!(
        world.fail_outcomes.last().map(String::as_str),
        Some("abandoned"),
        "the task was not abandoned within its budget: {:?}",
        world.fail_outcomes
    );
}

#[then("the job reports failed with one abandoned task")]
async fn job_reports_failed(world: &mut ControlWorld) {
    let uri = format!("/api/v1/jobs/{}", world.job_id());
    let (status, body) = world.get(&uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], json!("failed"), "status body: {body}");
    assert_eq!(body["abandoned"], json!(1), "status body: {body}");
}

#[then("the second submission resumes the existing job rather than creating a new one")]
fn second_submission_resumes(world: &mut ControlWorld) {
    let (first_status, first) = world.first_submission.as_ref().expect("a first submission");
    let (second_status, second) = world.submitted();
    assert_eq!(*first_status, StatusCode::CREATED);
    assert_eq!(first["created"], json!(true));
    assert_eq!(*second_status, StatusCode::OK, "body: {second}");
    assert_eq!(second["created"], json!(false));
    assert_eq!(second["job_id"], first["job_id"]);
}

#[tokio::main]
async fn main() {
    // An undefined step is skipped, not failed, and would still exit 0;
    // `fail_on_skipped` makes a renamed step a visible failure.
    ControlWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("features")
        .await;
}
