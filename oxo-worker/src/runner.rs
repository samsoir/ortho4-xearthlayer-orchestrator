//! The runner contract and process supervision.
//!
//! The runner is an external program (the Ortho4XP wrapper). The worker
//! writes one JSON line of [`RunnerInput`] to its stdin, closes it, and reads
//! one JSON [`RunnerResult`] line from the end of its stdout. Stderr is
//! inherited so Ortho4XP chatter lands in the worker's logs. Exit status is
//! trusted only together with a parseable result line.
//!
//! No timers live here: the loop task decides when to give up and calls
//! [`TaskRun::kill_and_reap`].

use std::collections::BTreeMap;
use std::io;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// What the runner is told, as one JSON line on stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerInput {
    pub tile: String,
    pub task_type: String,
    pub config: serde_json::Value,
    pub install_root: String,
    pub overlay_src: String,
    /// Pod-level Ortho4XP app-variable overrides; strings end to end, in
    /// deterministic order, converted by the runner like `raw`.
    pub app_overrides: BTreeMap<String, String>,
}

/// What the runner reports, as the last non-empty stdout line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum RunnerResult {
    Ok,
    Failed { reason: String, phase: String },
}

/// The supervisor's verdict on one runner invocation.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Success,
    Failed {
        reason: String,
        phase: Option<String>,
    },
}

/// Parse the last non-empty line of the runner's stdout, tolerating noise
/// before it.
pub fn parse_result(stdout: &str) -> Option<RunnerResult> {
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    serde_json::from_str(line.trim()).ok()
}

/// Combine exit status and stdout into a verdict.
pub fn judge(code: Option<i32>, stdout: &str) -> RunOutcome {
    let fail = |reason: String| RunOutcome::Failed {
        reason,
        phase: None,
    };
    match (code, parse_result(stdout)) {
        (Some(0), Some(RunnerResult::Ok)) => RunOutcome::Success,
        (_, Some(RunnerResult::Failed { reason, phase })) => RunOutcome::Failed {
            reason,
            phase: Some(phase),
        },
        (Some(c), Some(RunnerResult::Ok)) => {
            fail(format!("runner exited {c} despite reporting ok"))
        }
        (None, Some(RunnerResult::Ok)) => {
            fail("runner killed by signal despite reporting ok".into())
        }
        (Some(c), None) => fail(format!("runner exited {c} without a result line")),
        (None, None) => fail("runner killed by signal".into()),
    }
}

/// A running runner process.
pub struct TaskRun {
    child: Child,
    stdout: Option<JoinHandle<String>>,
    /// The runner leads its own process group; its pid is the group id.
    pgid: Option<u32>,
}

/// Linux errno 26; `ErrorKind::ExecutableFileBusy` is newer than our MSRV.
const ETXTBSY: i32 = 26;

/// Spawn the runner, tolerating ETXTBSY. A script written moments before
/// being executed can still have its write fd inherited by a concurrently
/// forked child in another thread (tests do exactly this), and exec then
/// fails with "text file busy" until that child execs. The window is
/// microseconds, so a few short retries is the standard remedy.
fn spawn_with_retry(cmd: &str) -> io::Result<Child> {
    let mut attempt = 0;
    loop {
        let spawned = Command::new(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            // Its own group, so a kill reaches Ortho4XP's subprocesses
            // (DSFTool and friends) and not only the direct child.
            .process_group(0)
            .spawn();
        match spawned {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// Spawn `cmd`, hand it `input` on stdin, and return the supervised handle.
pub async fn run_task(cmd: &str, input: &RunnerInput) -> io::Result<TaskRun> {
    let mut child = spawn_with_retry(cmd)?;
    let pgid = child.id();

    let mut line = serde_json::to_vec(input).map_err(io::Error::other)?;
    line.push(b'\n');
    if let Some(mut stdin) = child.stdin.take() {
        // A runner that exits before reading breaks the pipe; its exit
        // status and result line are the diagnostic, not this error.
        let _ = stdin.write_all(&line).await;
        let _ = stdin.shutdown().await;
    }

    let mut out = child.stdout.take().expect("stdout is piped");
    let reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    Ok(TaskRun {
        child,
        stdout: Some(reader),
        pgid,
    })
}

impl TaskRun {
    /// Wait for the runner to finish and judge the result. Cancel-safe at
    /// every await: dropping the future loses nothing, the child stays
    /// owned and the stdout drain handle stays in `self` until it has
    /// actually completed, so a later call still sees the whole output.
    pub async fn wait(&mut self) -> io::Result<RunOutcome> {
        let status = self.child.wait().await?;
        let stdout = match self.stdout.as_mut() {
            Some(h) => {
                let out = h.await.unwrap_or_default();
                self.stdout = None;
                out
            }
            None => String::new(),
        };
        Ok(judge(status.code(), &stdout))
    }

    /// Kill the runner and reap it, leaving no zombie. Used on lease loss.
    pub async fn kill_and_reap(&mut self) -> io::Result<()> {
        if let Some(h) = self.stdout.take() {
            h.abort();
        }
        // Signal the whole group first (while the leader is unreaped, so
        // the group id cannot have been recycled); ESRCH means it is gone.
        if let Some(pgid) = self
            .pgid
            .and_then(|p| rustix::process::Pid::from_raw(p as i32))
        {
            let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
        }
        // `kill` signals and then awaits the child.
        match self.child.kill().await {
            Ok(()) => Ok(()),
            // Already exited and reaped.
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The child's pid while it is running.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn input() -> RunnerInput {
        RunnerInput {
            tile: "+50+006".into(),
            task_type: "ortho".into(),
            config: serde_json::json!({"zl": 16}),
            install_root: "/opt/o4xp".into(),
            overlay_src: "/xp/overlay".into(),
            app_overrides: BTreeMap::new(),
        }
    }

    async fn run(stub: &str) -> RunOutcome {
        let mut r = run_task(&fx(stub), &input()).await.unwrap();
        r.wait().await.unwrap()
    }

    #[tokio::test]
    async fn success_line_and_exit_zero_is_success() {
        assert_eq!(run("stub-runner.sh").await, RunOutcome::Success);
    }

    #[tokio::test]
    async fn failed_line_carries_reason_and_phase() {
        assert_eq!(
            run("stub-runner-fail.sh").await,
            RunOutcome::Failed {
                reason: "mesh exploded".into(),
                phase: Some("build_mesh".into())
            }
        );
    }

    #[tokio::test]
    async fn nonzero_without_a_line_gets_a_synthesized_reason() {
        assert_eq!(
            run("stub-runner-silent-fail.sh").await,
            RunOutcome::Failed {
                reason: "runner exited 7 without a result line".into(),
                phase: None
            }
        );
    }

    #[tokio::test]
    async fn large_stdout_is_drained_without_deadlock_or_truncation() {
        assert_eq!(run("stub-runner-noisy.sh").await, RunOutcome::Success);
    }

    #[tokio::test]
    async fn input_arrives_as_one_json_line_on_stdin() {
        match run("stub-runner-echo.sh").await {
            RunOutcome::Failed { reason, .. } => {
                let got: RunnerInput = serde_json::from_str(&reason).unwrap();
                assert_eq!(got, input());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hung_runner_is_killed_and_reaped_through_the_handle() {
        let mut r = run_task(&fx("stub-runner-hang.sh"), &input())
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tx.send(()).unwrap();
        tokio::select! {
            _ = r.wait() => panic!("hang stub must not finish"),
            _ = rx => r.kill_and_reap().await.unwrap(),
        }
        // Reaped: a second kill is a harmless no-op and the pid is gone.
        r.kill_and_reap().await.unwrap();
        assert!(r.id().is_none());
    }

    fn script(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("stub.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn a_cancelled_wait_after_exit_does_not_lose_the_output() {
        // The runner reports ok and exits at once, but a background child
        // keeps the stdout pipe open for a while, so the drain is still
        // pending after the exit. Cancelling wait() there (as a heartbeat
        // tick does) must not lose the drain.
        let d = tempfile::tempdir().unwrap();
        let stub = script(
            d.path(),
            r#"read -r _l; sleep 0.3 & echo '{"outcome":"ok"}'"#,
        );
        let mut r = run_task(&stub, &input()).await.unwrap();
        let outcome = loop {
            tokio::select! {
                o = r.wait() => break o.unwrap(),
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            }
        };
        assert_eq!(outcome, RunOutcome::Success);
    }

    #[tokio::test]
    async fn killing_the_runner_kills_its_grandchildren() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("gc.pid");
        let stub = script(
            d.path(),
            &format!("sleep 100000 &\necho $! > {}\nwait", pidfile.display()),
        );
        let mut r = run_task(&stub, &input()).await.unwrap();
        let mut pid = String::new();
        for _ in 0..500 {
            pid = std::fs::read_to_string(&pidfile).unwrap_or_default();
            if pid.ends_with('\n') {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let pid = pid.trim().to_string();
        assert!(!pid.is_empty());
        let proc = format!("/proc/{pid}");
        assert!(std::path::Path::new(&proc).exists());
        r.kill_and_reap().await.unwrap();
        // The orphaned grandchild is reparented and reaped by init; allow
        // it a moment.
        for _ in 0..500 {
            let gone = std::fs::read_to_string(format!("{proc}/stat"))
                .map(|s| s.contains(") Z"))
                .unwrap_or(true);
            if gone {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("grandchild survived the kill");
    }

    #[test]
    fn judge_covers_noise_signal_and_zero_without_line() {
        let noisy = "chatter\n{\"outcome\":\"ok\"}\n\n";
        assert_eq!(judge(Some(0), noisy), RunOutcome::Success);
        assert_eq!(
            judge(None, ""),
            RunOutcome::Failed {
                reason: "runner killed by signal".into(),
                phase: None
            }
        );
        assert!(matches!(judge(Some(0), "junk"), RunOutcome::Failed { .. }));
        let ok = r#"{"outcome":"ok"}"#;
        let failed = r#"{"outcome":"failed","reason":"r","phase":"p"}"#;
        assert_eq!(
            judge(Some(3), ok),
            RunOutcome::Failed {
                reason: "runner exited 3 despite reporting ok".into(),
                phase: None
            }
        );
        assert_eq!(
            judge(None, ok),
            RunOutcome::Failed {
                reason: "runner killed by signal despite reporting ok".into(),
                phase: None
            }
        );
        // last line wins, in both orders
        let failed_outcome = RunOutcome::Failed {
            reason: "r".into(),
            phase: Some("p".into()),
        };
        assert_eq!(judge(Some(0), &format!("{ok}\n{failed}\n")), failed_outcome);
        assert_eq!(
            judge(Some(0), &format!("{failed}\n{ok}\n")),
            RunOutcome::Success
        );
        // exit 0 but a failed line is still a failure
        assert!(matches!(
            judge(Some(0), r#"{"outcome":"failed","reason":"r","phase":"p"}"#),
            RunOutcome::Failed { .. }
        ));
    }
}
