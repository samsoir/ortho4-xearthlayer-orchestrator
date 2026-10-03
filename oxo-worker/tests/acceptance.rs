//! Acceptance scenarios for the worker pod: the real claim loop against the
//! real router (in-process, in-memory store, ephemeral port), driving stub
//! runner scripts over tempdir trees. No podman, and no wall clock beyond the
//! tiny injected intervals.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use cucumber::{given, then, when, World};
use oxo_tasks::{InMemoryTaskStore, TestClock};
use oxo_worker::api::ControlPlane;
use oxo_worker::config::Config;
use oxo_worker::run::{run, Deps, ExitReason};
use serde_json::Value;
use tokio::net::TcpListener;

const TILES: [&str; 2] = ["+50-002", "+51-002"];

#[derive(Debug, World)]
#[world(init = Self::new)]
struct WorkerWorld {
    dir: tempfile::TempDir,
    base: String,
    job_id: String,
    exit: Option<ExitReason>,
}

impl WorkerWorld {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
            base: String::new(),
            job_id: String::new(),
            exit: None,
        }
    }

    fn target(&self) -> PathBuf {
        self.dir.path().join("artifacts")
    }

    fn scratch(&self) -> PathBuf {
        self.dir.path().join("scratch")
    }

    /// Serve the real router over a fresh in-memory store and submit a spec.
    async fn submit(&mut self, tiles: &[&str], overlays: bool, attempts: u32) {
        std::fs::create_dir_all(self.target()).expect("target root");
        let spec = format!(
            "tiles = [{tiles}]\n\
             \n\
             [metadata]\n\
             name = \"Acceptance region\"\n\
             region_code = \"AC\"\n\
             revision = 1\n\
             \n\
             [parameters]\n\
             provider = \"BI\"\n\
             zoom = 16\n\
             include_overlays = {overlays}\n\
             \n\
             [target]\n\
             root = \"{root}\"\n\
             \n\
             [failure_policy]\n\
             max_attempts = {attempts}\n\
             backoff_seconds = 0\n",
            tiles = tiles
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", "),
            root = self.target().display(),
        );
        let clock = Arc::new(TestClock::new(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp"),
        ));
        let store = Arc::new(InMemoryTaskStore::new(clock));
        let app = oxo_control::api::router(store);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        self.base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let created: Value = reqwest::Client::new()
            .post(format!("{}/api/v1/jobs", self.base))
            .body(spec)
            .send()
            .await
            .expect("submit")
            .json()
            .await
            .expect("submission body");
        self.job_id = created["job_id"]
            .as_str()
            .unwrap_or_else(|| panic!("submission refused: {created}"))
            .to_string();
    }

    async fn job(&self) -> Value {
        reqwest::get(format!("{}/api/v1/jobs/{}", self.base, self.job_id))
            .await
            .expect("status")
            .json()
            .await
            .expect("status body")
    }

    fn script(&self, name: &str, body: &str) -> String {
        let p = self.dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        p.to_string_lossy().into_owned()
    }

    /// A runner that delivers what a real build would leave in scratch:
    /// the tile directory for ortho, the `.dsf` inside the (pre-created)
    /// block directory for overlay.
    fn building_runner(&self) -> String {
        let s = self.scratch().display().to_string();
        self.script(
            "build.sh",
            &format!(
                r#"read -r line
tile=$(printf '%s' "$line" | sed 's/.*"tile":"\([^"]*\)".*/\1/')
case "$line" in
  *'"task_type":"overlay"'*)
    for d in "{s}/yOrtho4XP_Overlays/Earth nav data"/*; do echo dsf > "$d/$tile.dsf"; done ;;
  *)
    mkdir -p "{s}/Tiles/zOrtho4XP_$tile"
    echo built > "{s}/Tiles/zOrtho4XP_$tile/tile.txt" ;;
esac
echo '{{"outcome":"ok"}}'"#
            ),
        )
    }

    fn failing_runner() -> String {
        format!(
            "{}/tests/fixtures/stub-runner-fail.sh",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    fn config(&self, runner: &str, mode: &str) -> Config {
        let content = self.dir.path().join("content");
        std::fs::create_dir_all(&content).expect("content");
        std::fs::create_dir_all(self.scratch()).expect("scratch");
        Config::try_parse_from([
            "oxo-worker",
            "--control-plane-url",
            &self.base,
            "--worker-name",
            "acceptance-worker",
            "--mode",
            mode,
            "--min-free-scratch-bytes",
            "0",
            "--scratch-dir",
            &self.scratch().to_string_lossy(),
            "--content-dir",
            &content.to_string_lossy(),
            "--patches-link",
            &self.dir.path().join("patches-active").to_string_lossy(),
            "--runner",
            runner,
        ])
        .expect("worker config")
    }

    fn deps() -> Deps {
        Deps {
            poll_interval: Duration::from_millis(5),
            heartbeat_interval: Duration::from_millis(5),
            free_space: Box::new(|| Ok(u64::MAX)),
        }
    }

    /// Run a recycling worker until the job leaves `in_progress`, then stop
    /// it. A recycling worker never exits by itself, so the job's own state
    /// is the signal; the bound is a count of short polls, not a deadline.
    async fn drain(&mut self, runner: &str) {
        let config = self.config(runner, "recycle");
        let client = ControlPlane::new(&self.base);
        let worker = tokio::spawn(async move { run(&config, &client, Self::deps()).await });
        for _ in 0..2000 {
            if self.job().await["state"] != "in_progress" {
                worker.abort();
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        worker.abort();
        panic!("job never settled: {}", self.job().await);
    }
}

#[given("a control plane holding a submitted two-tile region with overlays")]
async fn two_tiles_with_overlays(world: &mut WorkerWorld) {
    world.submit(&TILES, true, 3).await;
}

#[given("a control plane holding a submitted one-tile region with two attempts and no backoff")]
async fn one_tile_two_attempts(world: &mut WorkerWorld) {
    world.submit(&TILES[..1], false, 2).await;
}

#[given("a control plane holding a submitted two-tile region without overlays")]
async fn two_tiles_without_overlays(world: &mut WorkerWorld) {
    world.submit(&TILES, false, 3).await;
}

#[when("a recycling worker runs until the queue is empty")]
async fn recycling_worker_drains(world: &mut WorkerWorld) {
    let runner = world.building_runner();
    world.drain(&runner).await;
}

#[when("a worker whose runner always fails runs until the queue is empty")]
async fn failing_worker_drains(world: &mut WorkerWorld) {
    world.drain(&WorkerWorld::failing_runner()).await;
}

#[when("a stop-mode worker runs once")]
async fn stop_worker_runs_once(world: &mut WorkerWorld) {
    let runner = world.building_runner();
    let config = world.config(&runner, "stop");
    let client = ControlPlane::new(&world.base);
    world.exit = Some(run(&config, &client, WorkerWorld::deps()).await);
}

#[then("every task's artifact is delivered under the region's target root")]
fn artifacts_delivered(world: &mut WorkerWorld) {
    for tile in TILES {
        let ortho = world.target().join(format!("zOrtho4XP_{tile}/tile.txt"));
        assert_eq!(
            std::fs::read_to_string(&ortho)
                .unwrap_or_else(|e| panic!("{}: {e}", ortho.display()))
                .trim(),
            "built"
        );
        // Both tiles lie in the 10-degree block +50-010.
        let dsf = world.target().join(format!(
            "yOrtho4XP_Overlays/Earth nav data/+50-010/{tile}.dsf"
        ));
        assert!(dsf.is_file(), "missing overlay {}", dsf.display());
    }
}

#[then("the job reports complete")]
async fn job_complete(world: &mut WorkerWorld) {
    let job = world.job().await;
    assert_eq!(job["state"], "complete", "status: {job}");
}

#[then("the job reports failed with one abandoned task")]
async fn job_failed(world: &mut WorkerWorld) {
    let job = world.job().await;
    assert_eq!(job["state"], "failed", "status: {job}");
    assert_eq!(job["abandoned"], 1, "status: {job}");
}

#[then("exactly one task is complete and the worker has exited")]
async fn one_task_done(world: &mut WorkerWorld) {
    assert_eq!(world.exit, Some(ExitReason::TaskDone));
    let job = world.job().await;
    assert_eq!(job["state"], "in_progress", "status: {job}");
    assert_eq!(job["succeeded"], 1, "status: {job}");
}

#[tokio::main]
async fn main() {
    // An undefined step is skipped, not failed, and would still exit 0;
    // `fail_on_skipped` makes a renamed step a visible failure.
    WorkerWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("features")
        .await;
}
