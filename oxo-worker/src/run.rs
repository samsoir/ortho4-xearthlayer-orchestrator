//! The claim loop: capacity check, claim, build under a heartbeat race,
//! egress, honest reporting, wholesale cleanup, then recycle or stop.
//!
//! Intervals and the free-space probe arrive through [`Deps`] so tests
//! drive the whole loop with millisecond timers and a fake probe.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::api::{ApiFailure, ClaimedTask, ControlPlane};
use crate::config::{Config, Mode};
use crate::exec::{self, ExecPaths};
use crate::runner::{self, RunOutcome, RunnerInput};

/// Why the loop returned. `main` maps these to process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Stop mode: one task finished and its scratch is wiped.
    TaskDone,
    /// Stop mode: nothing was claimable.
    QueueDrained,
    /// The contract is broken or the pod's mounts are unusable; retrying
    /// cannot help.
    Misconfigured,
}

impl ExitReason {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::TaskDone | Self::QueueDrained => 0,
            Self::Misconfigured => 2,
        }
    }
}

/// Probe for the bytes free on the scratch volume.
pub type FreeSpace = Box<dyn Fn() -> io::Result<u64> + Send + Sync>;

/// The loop's injected collaborators.
pub struct Deps {
    pub poll_interval: Duration,
    pub heartbeat_interval: Duration,
    pub free_space: FreeSpace,
}

impl Deps {
    /// Production wiring: real intervals from the config and `statvfs` on
    /// the scratch directory.
    pub fn from_config(config: &Config) -> Self {
        let scratch = PathBuf::from(&config.scratch_dir);
        Self {
            poll_interval: Duration::from_secs(config.poll_interval_secs),
            heartbeat_interval: Duration::from_secs(config.heartbeat_interval_secs),
            free_space: Box::new(move || {
                let s = rustix::fs::statvfs(&scratch)?;
                Ok(s.f_bavail.saturating_mul(s.f_frsize))
            }),
        }
    }
}

/// How many times a report is attempted when the control plane is
/// momentarily unavailable.
const REPORT_ATTEMPTS: u32 = 3;

const ALL_TYPES: [&str; 2] = ["ortho", "overlay"];
const OVERLAY_ONLY: [&str; 1] = ["overlay"];

/// What happened to a task, for the loop's own bookkeeping.
enum Settled {
    /// Reported (or deliberately not): carry on.
    Done,
    /// The control plane refused us outright.
    Misconfigured,
}

pub async fn run(config: &Config, client: &ControlPlane, deps: Deps) -> ExitReason {
    let paths = ExecPaths {
        scratch: PathBuf::from(&config.scratch_dir),
        content: PathBuf::from(&config.content_dir),
    };
    for (what, dir) in [("scratch", &paths.scratch), ("content", &paths.content)] {
        if !dir.is_dir() {
            tracing::error!(%what, path = %dir.display(), "mount is not a directory");
            return ExitReason::Misconfigured;
        }
    }
    // Start from a known-empty skeleton, whatever a previous life left.
    if let Err(e) = exec::cleanup(&paths.scratch) {
        tracing::error!(error = %e, "cannot initialise scratch");
        return ExitReason::Misconfigured;
    }

    let worker = config.worker_name();
    tracing::info!(
        "worker {worker} polling {} mode {:?}",
        config.control_plane_url,
        config.mode
    );
    loop {
        // A probe that fails is treated as no room: claim only the work
        // that needs none.
        let roomy = (deps.free_space)()
            .map(|free| free >= config.min_free_scratch_bytes)
            .unwrap_or(false);
        // The explicit list is intended: this worker claims only the types
        // it can execute, so a task type added later is deliberately not
        // claimed until the worker learns it.
        let types: &[&str] = if roomy { &ALL_TYPES } else { &OVERLAY_ONLY };

        match client.claim(&worker, types).await {
            Ok(None) => match config.mode {
                Mode::Stop => return ExitReason::QueueDrained,
                Mode::Recycle => tokio::time::sleep(deps.poll_interval).await,
            },
            Err(ApiFailure::Fatal(status, body)) => {
                tracing::error!(status, %body, "claim refused");
                return ExitReason::Misconfigured;
            }
            // A 409 on a claim is outside the contract; treat it as the
            // transient it most plausibly is.
            Err(ApiFailure::Retryable(e) | ApiFailure::LeaseGone(e)) => {
                tracing::warn!(error = %e, "claim failed; retrying");
                tokio::time::sleep(deps.poll_interval).await;
            }
            Ok(Some(task)) => {
                tracing::info!(
                    "claimed task {} {} {} attempt {}",
                    task.task_id,
                    task.task_type,
                    task.tile,
                    task.attempt
                );
                let settled = work(config, client, &deps, &paths, &task).await;
                if let Err(e) = exec::cleanup(&paths.scratch) {
                    tracing::error!(error = %e, "cleanup failed; scratch is not clean");
                    return ExitReason::Misconfigured;
                }
                if matches!(settled, Settled::Misconfigured) {
                    return ExitReason::Misconfigured;
                }
                if config.mode == Mode::Stop {
                    return ExitReason::TaskDone;
                }
            }
        }
    }
}

/// One claimed task from prepare to report. Never cleans up; the caller does.
async fn work(
    config: &Config,
    client: &ControlPlane,
    deps: &Deps,
    paths: &ExecPaths,
    task: &ClaimedTask,
) -> Settled {
    if let Err(e) = exec::prepare(task, paths) {
        return report_fail(client, deps, task, &format!("prepare: {e}")).await;
    }

    let input = RunnerInput {
        tile: task.tile.clone(),
        task_type: task.task_type.clone(),
        config: task.config.clone(),
        install_root: config.install_root.clone(),
        overlay_src: config.overlay_src.clone(),
        app_overrides: config.o4_app_overrides.clone(),
    };
    let mut run = match runner::run_task(&config.runner, &input).await {
        Ok(run) => run,
        Err(e) => {
            return report_fail(client, deps, task, &format!("cannot start runner: {e}")).await
        }
    };

    let outcome = loop {
        tokio::select! {
            result = run.wait() => break result,
            _ = tokio::time::sleep(deps.heartbeat_interval) => {
                match client.heartbeat(task.task_id, task.lease_token).await {
                    Ok(()) => {}
                    Err(ApiFailure::LeaseGone(why)) => {
                        // Not ours any more: stop the build, say nothing.
                        tracing::warn!(
                            task = %task.task_id, %why,
                            "lease lost for task {}; killed runner, no report", task.task_id
                        );
                        if let Err(e) = run.kill_and_reap().await {
                            tracing::error!(error = %e, "could not reap the runner");
                        }
                        return Settled::Done;
                    }
                    Err(ApiFailure::Retryable(e)) => {
                        tracing::warn!(error = %e, "heartbeat failed; next tick retries");
                    }
                    Err(ApiFailure::Fatal(status, body)) => {
                        tracing::error!(status, %body, "heartbeat refused");
                        let _ = run.kill_and_reap().await;
                        return Settled::Misconfigured;
                    }
                }
            }
        }
    };

    match outcome {
        Ok(RunOutcome::Success) => match exec::egress(task, paths) {
            Ok(()) => report_complete(client, deps, task).await,
            Err(e) => report_fail(client, deps, task, &format!("egress: {e}")).await,
        },
        Ok(RunOutcome::Failed { reason, phase }) => {
            let reason = match phase {
                Some(phase) => format!("{phase}: {reason}"),
                None => reason,
            };
            report_fail(client, deps, task, &reason).await
        }
        Err(e) => {
            let _ = run.kill_and_reap().await;
            report_fail(client, deps, task, &format!("runner supervision: {e}")).await
        }
    }
}

async fn report_complete(client: &ControlPlane, deps: &Deps, task: &ClaimedTask) -> Settled {
    report(deps, task, "complete", None, || {
        client.complete(task.task_id, task.lease_token)
    })
    .await
}

async fn report_fail(
    client: &ControlPlane,
    deps: &Deps,
    task: &ClaimedTask,
    reason: &str,
) -> Settled {
    report(deps, task, "fail", Some(reason), || async {
        client
            .fail(task.task_id, task.lease_token, reason)
            .await
            .map(|_| ())
    })
    .await
}

/// A report with bounded retries. A 409 means the lease is gone and there
/// is nothing left to say.
async fn report<F, Fut>(
    deps: &Deps,
    task: &ClaimedTask,
    verb: &str,
    detail: Option<&str>,
    call: F,
) -> Settled
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), ApiFailure>>,
{
    for attempt in 1..=REPORT_ATTEMPTS {
        match call().await {
            Ok(()) => {
                match detail {
                    Some(reason) => {
                        tracing::info!("failed task {}: {reason}", task.task_id)
                    }
                    None => tracing::info!("completed task {}", task.task_id),
                }
                return Settled::Done;
            }
            Err(ApiFailure::LeaseGone(_)) => return Settled::Done,
            Err(ApiFailure::Fatal(status, body)) => {
                tracing::error!(status, %body, verb, "report refused");
                return Settled::Misconfigured;
            }
            Err(ApiFailure::Retryable(e)) => {
                tracing::warn!(task = %task.task_id, attempt, error = %e, verb, "report failed");
                if attempt < REPORT_ATTEMPTS {
                    tokio::time::sleep(deps.poll_interval).await;
                }
            }
        }
    }
    tracing::error!(task = %task.task_id, verb, "giving up on report; the lease will expire");
    Settled::Done
}
