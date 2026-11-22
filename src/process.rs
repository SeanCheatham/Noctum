//! Timed process execution. Long commands run in their own process group so a
//! timeout can signal the whole tree.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub success: bool,
    pub timed_out: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CmdOutput {
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

pub async fn run(
    cwd: &Path,
    program: impl AsRef<OsStr>,
    args: &[impl AsRef<OsStr>],
    env: &[(&str, &str)],
    timeout: Duration,
) -> CmdOutput {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    spawn(command, timeout).await
}

pub async fn sh(cwd: &Path, script: &str, timeout: Duration) -> CmdOutput {
    run(cwd, "sh", &["-c", script], &[], timeout).await
}

async fn spawn(mut command: Command, timeout: Duration) -> CmdOutput {
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CmdOutput {
                success: false,
                timed_out: false,
                code: None,
                stdout: String::new(),
                stderr: format!("failed to spawn command: {err}"),
                duration_ms: elapsed(started),
            };
        }
    };
    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Reads run beside the wait. Dropping a Child while a read is still
    // joined to it can stall the runtime, so the child stays in this task.
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let (finished, timed_out) = tokio::select! {
        status = child.wait() => (status, false),
        _ = tokio::time::sleep(timeout) => {
            signal_group(pid, "-KILL");
            let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            (status, true)
        }
    };
    let (out, err) = pipes(stdout_task, stderr_task).await;

    match finished {
        Ok(status) => CmdOutput {
            success: status.success() && !timed_out,
            timed_out,
            code: status.code(),
            stdout: lossy(&out),
            stderr: if timed_out {
                format!("timed out after {} seconds", timeout.as_secs())
            } else {
                lossy(&err)
            },
            duration_ms: elapsed(started),
        },
        Err(err) => CmdOutput {
            success: false,
            timed_out,
            code: None,
            stdout: lossy(&out),
            stderr: if timed_out {
                format!("timed out after {} seconds", timeout.as_secs())
            } else {
                err.to_string()
            },
            duration_ms: elapsed(started),
        },
    }
}

async fn read_pipe(mut pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
}

async fn pipes(
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) -> (Vec<u8>, Vec<u8>) {
    let joined = tokio::time::timeout(Duration::from_secs(2), async {
        let out = stdout_task.await.unwrap_or_default();
        let err = stderr_task.await.unwrap_or_default();
        (out, err)
    })
    .await;
    joined.unwrap_or_default()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

pub fn signal_group(pid: u32, signal: &str) {
    if pid == 0 {
        return;
    }
    // A child in our own process group must be signaled by pid. Signaling
    // that group would kill the daemon.
    let own = process_group_id(std::process::id());
    let child_group = process_group_id(pid);
    let target = if child_group == Some(pid) && own != Some(pid) {
        format!("-{pid}")
    } else {
        pid.to_string()
    };
    let name = signal.trim_start_matches('-');
    let _ = std::process::Command::new("kill")
        .args(["-s", name, "--", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
fn process_group_id(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(not(target_os = "macos"))]
fn process_group_id(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse().ok()
}

/// Line tables, one codegen unit, and a frozen timestamp.
///
/// `cargo test --release` selects the release profile. `cargo bench` selects
/// the bench profile. Both are set so the harness stays symbolizable either way.
pub fn bench_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("CARGO_PROFILE_RELEASE_DEBUG", "line-tables-only"),
        ("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1"),
        ("CARGO_PROFILE_RELEASE_STRIP", "none"),
        ("CARGO_PROFILE_BENCH_DEBUG", "line-tables-only"),
        ("CARGO_PROFILE_BENCH_CODEGEN_UNITS", "1"),
        ("CARGO_PROFILE_BENCH_STRIP", "none"),
        ("SOURCE_DATE_EPOCH", "0"),
        ("CARGO_TERM_COLOR", "never"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let output = run(dir.path(), "echo", &["hello"], &[], Duration::from_secs(5)).await;
        assert!(output.success);
        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn timeout_kills_a_sleep() {
        let dir = tempfile::tempdir().unwrap();
        let output = run(dir.path(), "sleep", &["30"], &[], Duration::from_secs(1)).await;
        assert!(output.timed_out);
        assert!(!output.success);
    }
}
