//! The three edges of a night: measure, infer, and the host check.
//!
//! Production uses QEMU, llama-server, and cargo. Tests substitute the first
//! two. The instruction count still comes from the measurement, never from a
//! constant inside the orchestrator.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::profile::ProfileDump;
use crate::repo_config::RepoConfig;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeasureRequest {
    pub worktree: std::path::PathBuf,
    #[serde(default)]
    pub working_directory: Option<std::path::PathBuf>,
    pub harness: std::path::PathBuf,
    pub guest_exe: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub scratch: std::path::PathBuf,
    pub memory_mb: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Measurement {
    pub total: u64,
    #[serde(default)]
    pub memory: Option<crate::optimization::MemoryStats>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub wall_ms: u64,
    pub dump: ProfileDump,
    pub map_start: u64,
    pub map_end: u64,
    pub load_bias: u64,
    pub serial_log: String,
}

impl Measurement {
    pub fn matches(&self, other: &Measurement) -> bool {
        self.memory == other.memory
            && self.total == other.total
            && comparable_stdout(&self.stdout) == comparable_stdout(&other.stdout)
            && self.stderr == other.stderr
            && self.exit_code == other.exit_code
    }
}

/// Libtest prints `finished in 0.01s` on the summary line. Under icount that
/// duration follows the instruction count, so a cheaper body changes the
/// string. The gate compares the result text and leaves the timer out.
pub fn comparable_stdout(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let marker = b"finished in ";
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let ending = if line.ends_with(b"\r\n") {
            2
        } else {
            usize::from(line.ends_with(b"\n"))
        };
        let content = &line[..line.len() - ending];
        let timer = content
            .windows(marker.len())
            .position(|part| part == marker);
        if let Some(index) = timer.filter(|_| content.starts_with(b"test result:")) {
            let duration = &content[index + marker.len()..];
            if let Some(number) = duration.strip_suffix(b"s") {
                if number.first().is_some_and(u8::is_ascii_digit)
                    && number.last().is_some_and(u8::is_ascii_digit)
                    && number.iter().all(|b| b.is_ascii_digit() || *b == b'.')
                    && number.iter().filter(|b| **b == b'.').count() <= 1
                {
                    out.extend_from_slice(&content[..index]);
                    out.extend_from_slice(b"finished in TIME");
                    out.extend_from_slice(&line[line.len() - ending..]);
                    continue;
                }
            }
        }
        out.extend_from_slice(line);
    }
    out
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub summary: String,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct CheckOutput {
    pub success: bool,
    pub timed_out: bool,
    pub output: String,
}

pub trait Measurer: Send + Sync {
    fn measure(
        &self,
        req: &MeasureRequest,
    ) -> impl std::future::Future<Output = anyhow::Result<Measurement>> + Send;
}

pub trait Inferencer: Send + Sync {
    fn complete(
        &self,
        messages: &[ChatMessage],
        cancel: &AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<Completion>> + Send;
}

pub trait HostCheck: Send + Sync {
    fn run(
        &self,
        worktree: &Path,
        repo: &RepoConfig,
        package: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<CheckOutput>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{comparable_stdout, Measurement};

    #[test]
    fn repeated_measurements_must_keep_error_output() {
        let first = Measurement {
            memory: None,
            total: 20_000,
            stdout: b"ok\n".to_vec(),
            stderr: vec![0, 255, 13, 10],
            exit_code: 0,
            wall_ms: 1,
            dump: crate::profile::ProfileDump {
                total: 20_000,
                blocks: Vec::new(),
                error: None,
            },
            map_start: 0,
            map_end: u64::MAX,
            load_bias: 0,
            serial_log: String::new(),
        };
        let mut second = first.clone();
        assert!(first.matches(&second));
        second.memory = Some(crate::optimization::MemoryStats {
            allocations: 1,
            allocated_bytes: 10,
        });
        assert!(!first.matches(&second));
        second.memory = None;
        second.stderr[1] = 254;
        assert!(!first.matches(&second));
    }

    #[test]
    fn output_comparison_keeps_non_utf8_bytes_and_plain_text_timers() {
        assert_ne!(comparable_stdout(b"\xff\n"), comparable_stdout(b"\xfe\n"));
        let plain = b"test result: input finished in a different state\n";
        assert_eq!(comparable_stdout(plain), plain);
        let binary = b"\xff\nfinished in 0.01s\n";
        assert_eq!(comparable_stdout(binary), binary);
    }

    #[test]
    fn libtest_duration_is_not_part_of_the_comparison() {
        let slow = b"test tests::encode_batch ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.42s\n";
        let fast = b"test tests::encode_batch ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        assert_eq!(comparable_stdout(slow), comparable_stdout(fast));
        let failed = b"test tests::encode_batch ... ok\ntest result: ok. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        assert_ne!(comparable_stdout(slow), comparable_stdout(failed));
    }
}
