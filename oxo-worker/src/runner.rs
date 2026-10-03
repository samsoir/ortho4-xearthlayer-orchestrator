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
    match (code, parse_result(stdout)) {
        (Some(0), Some(RunnerResult::Ok)) => RunOutcome::Success,
        (_, Some(RunnerResult::Failed { reason, phase })) => RunOutcome::Failed {
            reason,
            phase: Some(phase),
        },
        (Some(0), None) => RunOutcome::Failed {
            reason: "runner exited 0 without a result line".into(),
            phase: None,
        },
        (Some(c), _) => RunOutcome::Failed {
            reason: format!("runner exited {c} without a result line"),
            phase: None,
        },
        (None, _) => RunOutcome::Failed {
            reason: "runner killed by signal".into(),
            phase: None,
        },
    }
}

/// A running runner process.
pub struct TaskRun {
    child: Child,
    stdout: Option<JoinHandle<String>>,
}

/// Spawn `cmd`, hand it `input` on stdin, and return the supervised handle.
pub async fn run_task(cmd: &str, input: &RunnerInput) -> io::Result<TaskRun> {
    let mut child = Command::new(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;

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
    })
}

impl TaskRun {
    /// Wait for the runner to finish and judge the result. Cancel-safe: if
    /// dropped mid-wait the process is still owned and can be killed.
    pub async fn wait(&mut self) -> io::Result<RunOutcome> {
        let status = self.child.wait().await?;
        let stdout = match self.stdout.take() {
            Some(h) => h.await.unwrap_or_default(),
            None => String::new(),
        };
        Ok(judge(status.code(), &stdout))
    }

    /// Kill the runner and reap it, leaving no zombie. Used on lease loss.
    pub async fn kill_and_reap(&mut self) -> io::Result<()> {
        if let Some(h) = self.stdout.take() {
            h.abort();
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
        // exit 0 but a failed line is still a failure
        assert!(matches!(
            judge(Some(0), r#"{"outcome":"failed","reason":"r","phase":"p"}"#),
            RunOutcome::Failed { .. }
        ));
    }
}
