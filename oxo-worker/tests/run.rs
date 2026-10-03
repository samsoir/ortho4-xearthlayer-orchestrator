//! The claim loop against the real router (in-process, in-memory store),
//! driving real stub runner processes over tempfile trees.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use oxo_control::api::router;
use oxo_tasks::{InMemoryTaskStore, ReapRequest, TaskStore, TestClock, TimeoutSeconds};
use oxo_worker::api::ControlPlane;
use oxo_worker::config::Config;
use oxo_worker::run::{run, Deps, ExitReason};
use tokio::net::TcpListener;

const SPEC: &str = r#"
tiles = ["+50-002", "+51-002"]

[metadata]
name = "North America"
region_code = "NA"
revision = 1

[parameters]
provider = "BI"
zoom = 16
include_overlays = false

[target]
root = "TARGET"

[failure_policy]
max_attempts = 2
backoff_seconds = 60
"#;

struct Env {
    dir: tempfile::TempDir,
    base: String,
    client: ControlPlane,
    store: Arc<InMemoryTaskStore>,
    clock: Arc<TestClock>,
    job_id: String,
}

impl Env {
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn scratch(&self) -> PathBuf {
        self.root().join("scratch")
    }
    fn target(&self) -> PathBuf {
        self.root().join("artifacts")
    }

    /// A stub runner script written into the tempdir.
    fn script(&self, name: &str, body: &str) -> String {
        let p = self.root().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// Builds the ortho deliverable in scratch, as the real runner would,
    /// after logging how many leftovers it found there.
    fn building_runner(&self) -> String {
        let s = self.scratch().display().to_string();
        let log = self.root().join("leftovers.log").display().to_string();
        self.script(
            "build.sh",
            &format!(
                r#"read -r line
tile=$(printf '%s' "$line" | sed 's/.*"tile":"\([^"]*\)".*/\1/')
ls "{s}/Tiles" | wc -l >> "{log}"
mkdir -p "{s}/Tiles/zOrtho4XP_$tile/terrain" "{s}/Tiles/zOrtho4XP_$tile/Earth nav data/+50+000"
echo dsf > "{s}/Tiles/zOrtho4XP_$tile/Earth nav data/+50+000/$tile.dsf"
echo built > "{s}/Tiles/zOrtho4XP_$tile/terrain/tile.ter"
echo '{{"outcome":"ok"}}'"#
            ),
        )
    }

    fn config(&self, runner: &str, mode: &str, min_free: u64) -> Config {
        let content = self.root().join("content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::create_dir_all(self.scratch()).unwrap();
        Config::try_parse_from([
            "oxo-worker",
            "--control-plane-url",
            &self.base,
            "--worker-name",
            "w1",
            "--mode",
            mode,
            "--min-free-scratch-bytes",
            &min_free.to_string(),
            "--scratch-dir",
            &self.scratch().to_string_lossy(),
            "--content-dir",
            &content.to_string_lossy(),
            "--runner",
            runner,
        ])
        .unwrap()
    }

    async fn job_state(&self) -> serde_json::Value {
        reqwest::get(format!("{}/api/v1/jobs/{}", self.base, self.job_id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn wait_for_state(&self, state: &str) {
        for _ in 0..2000 {
            if self.job_state().await["state"] == state {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("job never became {state}: {}", self.job_state().await);
    }
}

fn deps(free: u64) -> Deps {
    Deps {
        poll_interval: Duration::from_millis(5),
        heartbeat_interval: Duration::from_millis(5),
        free_space: Box::new(move || Ok(free)),
    }
}

async fn env_with(spec_edit: impl Fn(String) -> String) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("artifacts");
    std::fs::create_dir_all(&target).unwrap();
    let spec = spec_edit(SPEC.replace("TARGET", &target.to_string_lossy()));
    let clock = Arc::new(TestClock::new(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    ));
    let store = Arc::new(InMemoryTaskStore::new(clock.clone()));
    let app = router(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let created: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/jobs"))
        .body(spec)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    Env {
        dir,
        client: ControlPlane::new(&base),
        base,
        store,
        clock,
        job_id: created["job_id"].as_str().unwrap().to_string(),
    }
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

fn assert_scratch_empty(env: &Env) {
    for sub in ["tmp", "Tiles", "yOrtho4XP_Overlays"] {
        assert_eq!(entries(&env.scratch().join(sub)), 0, "{sub}");
    }
}

#[tokio::test]
async fn a_recycle_worker_drains_a_two_task_job() {
    let env = env_with(|s| s).await;
    let runner = env.building_runner();
    let config = env.config(&runner, "recycle", 0);
    let client = env.client.clone();
    let worker = tokio::spawn(async move { run(&config, &client, deps(u64::MAX)).await });

    env.wait_for_state("complete").await;
    worker.abort();

    for tile in ["+50-002", "+51-002"] {
        let f = env
            .target()
            .join(format!("zOrtho4XP_{tile}/terrain/tile.ter"));
        assert_eq!(std::fs::read_to_string(f).unwrap().trim(), "built");
    }
    let log = std::fs::read_to_string(env.root().join("leftovers.log")).unwrap();
    assert_eq!(
        log.split_whitespace().collect::<Vec<_>>(),
        ["0", "0"],
        "scratch was dirty at a task's start"
    );
}

#[tokio::test]
async fn stop_mode_takes_exactly_one_task() {
    let env = env_with(|s| s).await;
    let runner = env.building_runner();
    let config = env.config(&runner, "stop", 0);
    let reason = run(&config, &env.client, deps(u64::MAX)).await;
    assert_eq!(reason, ExitReason::TaskDone);
    assert_eq!(env.job_state().await["succeeded"], 1);
    assert_eq!(entries(&env.target()), 1);
    assert_scratch_empty(&env);
}

#[tokio::test]
async fn stop_mode_on_an_empty_queue_reports_the_drain() {
    let env = env_with(|s| {
        s.replace(
            r#"tiles = ["+50-002", "+51-002"]"#,
            r#"tiles = ["+50-002"]"#,
        )
    })
    .await;
    let runner = env.building_runner();
    let config = env.config(&runner, "stop", 0);
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::TaskDone
    );
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::QueueDrained
    );
}

#[tokio::test]
async fn a_failing_runner_burns_the_budget_to_a_failed_job() {
    let env = env_with(|s| {
        s.replace(
            r#"tiles = ["+50-002", "+51-002"]"#,
            r#"tiles = ["+50-002"]"#,
        )
    })
    .await;
    let runner = format!(
        "{}/tests/fixtures/stub-runner-fail.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let config = env.config(&runner, "stop", 0);
    // max_attempts = 2: two failures, the second after the backoff.
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::TaskDone
    );
    env.clock.advance(Duration::from_secs(3600));
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::TaskDone
    );
    assert_eq!(env.job_state().await["state"], "failed");
    assert_scratch_empty(&env);
}

fn pid_alive(pid: &str) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test]
async fn a_lost_lease_kills_the_child_and_the_loop_carries_on() {
    let env = env_with(|s| {
        s.replace(
            r#"tiles = ["+50-002", "+51-002"]"#,
            r#"tiles = ["+50-002"]"#,
        )
    })
    .await;
    let pids = env.root().join("pids.log");
    let runner = env.script(
        "hang.sh",
        &format!(r#"echo $$ >> "{}"; exec sleep 100000"#, pids.display()),
    );
    let config = env.config(&runner, "recycle", 0);
    let client = env.client.clone();
    let worker = tokio::spawn(async move { run(&config, &client, deps(u64::MAX)).await });

    let read_pids = || -> Vec<String> {
        std::fs::read_to_string(&pids)
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect()
    };
    let wait_pids = |n: usize| async move {
        for _ in 0..2000 {
            if read_pids().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("runner never started {n} times");
    };
    wait_pids(1).await;
    let first = read_pids()[0].clone();
    assert!(pid_alive(&first));

    env.clock.advance(Duration::from_secs(600));
    env.store
        .reap_expired(ReapRequest {
            heartbeat_timeout: TimeoutSeconds::new(30).unwrap(),
            max_task_duration: TimeoutSeconds::new(86_400).unwrap(),
        })
        .await
        .unwrap();

    for _ in 0..2000 {
        if !pid_alive(&first) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!pid_alive(&first), "child outlived its lease");

    // The task is re-claimable (after backoff) and this same loop took it
    // again: it carried on rather than exiting or wedging.
    env.clock.advance(Duration::from_secs(3600));
    wait_pids(2).await;
    assert!(!worker.is_finished());
    worker.abort();
}

#[tokio::test]
async fn low_scratch_claims_overlay_work_only() {
    let env = env_with(|s| {
        s.replace("include_overlays = false", "include_overlays = true")
            .replace(
                r#"tiles = ["+50-002", "+51-002"]"#,
                r#"tiles = ["+50-002"]"#,
            )
    })
    .await;
    let seen = env.root().join("seen.log");
    let runner = env.script(
        "record.sh",
        &format!(
            r#"read -r line; echo "$line" >> "{}"; echo '{{"outcome":"ok"}}'"#,
            seen.display()
        ),
    );
    let config = env.config(&runner, "stop", 1000);
    assert_eq!(
        run(&config, &env.client, deps(999)).await,
        ExitReason::TaskDone
    );
    let line = std::fs::read_to_string(&seen).unwrap();
    assert!(line.contains(r#""task_type":"overlay""#), "{line}");

    // The overlay is spent and the ortho task is still pending, yet a
    // worker without room finds the queue drained: it never claims ortho.
    assert_eq!(
        run(&config, &env.client, deps(999)).await,
        ExitReason::QueueDrained
    );

    // Room again: the ortho task is claimed.
    let config = env.config(&runner, "stop", 1000);
    assert_eq!(
        run(&config, &env.client, deps(1000)).await,
        ExitReason::TaskDone
    );
    let lines = std::fs::read_to_string(&seen).unwrap();
    assert!(lines.contains(r#""task_type":"ortho""#), "{lines}");
}

#[tokio::test]
async fn a_slow_stdout_drain_never_turns_success_into_failure() {
    let env = env_with(|s| {
        s.replace(
            r#"tiles = ["+50-002", "+51-002"]"#,
            r#"tiles = ["+50-002"]"#,
        )
    })
    .await;
    let s = env.scratch().display().to_string();
    // Exits 0 at once, but a background child holds stdout open, so the
    // drain outlives several heartbeat ticks.
    let runner = env.script(
        "slow.sh",
        &format!(
            r#"read -r line
mkdir -p "{s}/Tiles/zOrtho4XP_+50-002/terrain" "{s}/Tiles/zOrtho4XP_+50-002/Earth nav data/+50-010"
echo dsf > "{s}/Tiles/zOrtho4XP_+50-002/Earth nav data/+50-010/+50-002.dsf"
echo built > "{s}/Tiles/zOrtho4XP_+50-002/terrain/tile.ter"
sleep 0.3 &
echo '{{"outcome":"ok"}}'"#
        ),
    );
    let config = env.config(&runner, "stop", 0);
    let mut d = deps(u64::MAX);
    d.heartbeat_interval = Duration::from_millis(5);
    assert_eq!(run(&config, &env.client, d).await, ExitReason::TaskDone);
    assert_eq!(env.job_state().await["state"], "complete");
}

#[tokio::test]
async fn pod_level_app_overrides_reach_the_runner_input() {
    let env = env_with(|s| s).await;
    let seen = env.root().join("seen.log");
    let runner = env.script(
        "record.sh",
        &format!(
            r#"read -r line; echo "$line" >> "{}"; echo '{{"outcome":"ok"}}'"#,
            seen.display()
        ),
    );
    let mut config = env.config(&runner, "stop", 0);
    config
        .o4_app_overrides
        .insert("max_download_slots".into(), "2".into());
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::TaskDone
    );
    let line = std::fs::read_to_string(&seen).unwrap();
    assert!(
        line.contains(r#""app_overrides":{"max_download_slots":"2"}"#),
        "{line}"
    );
}

#[tokio::test]
async fn the_config_overlay_is_in_place_by_the_time_the_first_task_runs() {
    let env = env_with(|s| s).await;
    let install = env.root().join("install");
    let overlay = env.root().join("overlay");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(install.join("overpass_servers.txt"), "public").unwrap();
    std::fs::write(overlay.join("overpass_servers.txt"), "local").unwrap();
    // The runner only starts after a claim, so this proves the overlay is
    // installed before the first TASK runs (not strictly before the first
    // claim; the refusal test below covers the pre-claim side).
    let seen = env.root().join("seen.log");
    let runner = env.script(
        "read_install.sh",
        &format!(
            r#"read -r line; cat "{}/overpass_servers.txt" >> "{}"; echo '{{"outcome":"ok"}}'"#,
            install.display(),
            seen.display()
        ),
    );
    let mut config = env.config(&runner, "stop", 0);
    config.install_root = install.to_string_lossy().into_owned();
    config.o4_config_overlay = Some(overlay.to_string_lossy().into_owned());
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::TaskDone
    );
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "local");
}

#[tokio::test]
async fn an_explicit_but_missing_config_overlay_refuses_startup() {
    let env = env_with(|s| s).await;
    let runner = env.building_runner();
    let mut config = env.config(&runner, "stop", 0);
    config.o4_config_overlay = Some(env.root().join("absent").to_string_lossy().into_owned());
    assert_eq!(
        run(&config, &env.client, deps(u64::MAX)).await,
        ExitReason::Misconfigured
    );
    // Refused before any claim: nothing was built or delivered.
    assert_eq!(entries(&env.target()), 0);
    assert_eq!(env.job_state().await["state"], "in_progress");
}
