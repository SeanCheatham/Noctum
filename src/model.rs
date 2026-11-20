//! llama.cpp owned by Noctum. No other local model server is installed or spoken to.
//!
//! The server is started for one completion and stopped before QEMU runs.
//! `-ngl 0` keeps the GPU out of it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::hashutil::{sha256_file, tail_str};
use crate::lockfile::{self, ModelLock};
use crate::ports::{ChatMessage, Completion, Inferencer};
use crate::process::signal_group;
use crate::profile::HotFunction;

const SCHEMA: &str = include_str!("../grammar/patch_schema.json");
const DOWNLOAD_ATTEMPTS: u32 = 3;
const HEALTH_WAIT: Duration = Duration::from_secs(300);
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct PromptInput<'a> {
    pub test_name: &'a str,
    pub test_source: &'a str,
    pub hot: &'a HotFunction,
    pub callee_sources: &'a [(String, String)],
    pub repair_diff: Option<&'a str>,
    pub repair_output: Option<&'a str>,
}

pub fn build_messages(input: &PromptInput<'_>) -> Vec<ChatMessage> {
    let system = "\
You reduce the number of guest instructions retired inside one single-threaded Rust test body. \
Preserve assertions, fixed loop bounds, and libtest success. Do not add dependencies. \
Do not edit noctum.toml, Cargo.toml, or Cargo.lock. Return a unified diff and a short summary. \
The fence that measures the body is not in the source. Do not delete the work of a fixed loop.";
    let mut user = format!(
        "Test: {}\nHot function: {}\nFile: {}\nLines: {}-{}\nExclusive instructions: {}\nShare of fenced total: {:.4}\n\nHot function source:\n```rust\n{}\n```\n\nTest source:\n```rust\n{}\n```\n",
        input.test_name,
        input.hot.name,
        input.hot.file,
        input.hot.line_start,
        input.hot.line_end,
        input.hot.exclusive,
        input.hot.share,
        input.hot.source,
        input.test_source
    );
    if input.hot.is_test {
        for (name, source) in input.callee_sources {
            user.push_str(&format!(
                "\nDirect callee {name}:\n```rust\n{source}\n```\n"
            ));
        }
    }
    if let Some(diff) = input.repair_diff {
        user.push_str("\nThe previous diff failed to build or failed the host test suite. Replace it. Do not repeat it.\n");
        user.push_str(diff);
        user.push('\n');
    }
    if let Some(output) = input.repair_output {
        user.push_str("\nTool output tail:\n");
        user.push_str(output);
        user.push('\n');
    }
    const CHAR_BUDGET: usize = 48_000;
    if user.len() > CHAR_BUDGET {
        user = format!(
            "Test: {}\nHot function: {}\nFile: {}\nLines: {}-{}\nExclusive instructions: {}\nShare of fenced total: {:.4}\n\nHot function source:\n```rust\n{}\n```\n\nTest source:\n```rust\n{}\n```\n",
            input.test_name,
            input.hot.name,
            input.hot.file,
            input.hot.line_start,
            input.hot.line_end,
            input.hot.exclusive,
            input.hot.share,
            input.hot.source,
            input.test_source
        );
    }
    vec![
        ChatMessage {
            role: "system".into(),
            content: system.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: user,
        },
    ]
}

pub struct LlamaManager {
    pub data_dir: PathBuf,
}

impl LlamaManager {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }
}

impl Inferencer for LlamaManager {
    async fn complete(&self, messages: &[ChatMessage], cancel: &AtomicBool) -> Result<Completion> {
        ensure_inference_resources(&self.data_dir)?;
        let mut last_error = None;
        for _ in 0..2 {
            if cancel.load(Ordering::SeqCst) {
                bail!("inference cancelled");
            }
            match oneshot(&self.data_dir, messages, cancel).await {
                Ok(completion) => return Ok(completion),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("llama-server failed")))
    }
}

fn ensure_inference_resources(data_dir: &Path) -> Result<()> {
    let lock = ModelLock::load()?;
    let gguf = lockfile::gguf_path(data_dir);
    if !gguf.is_file() {
        bail!("model file is missing");
    }
    let available = mem_available()?;
    let need = lock.gguf_bytes + 1024 * 1024 * 1024;
    if available < need {
        bail!(
            "need {} bytes of available RAM for the model, have {available}",
            need
        );
    }
    Ok(())
}

async fn oneshot(
    data_dir: &Path,
    messages: &[ChatMessage],
    cancel: &AtomicBool,
) -> Result<Completion> {
    let port = ephemeral_port()?;
    let lock = ModelLock::load()?;
    let threads = physical_cores().saturating_sub(1).max(1);
    let server = lockfile::server_path(data_dir);
    let gguf = lockfile::gguf_path(data_dir);
    let mut child = tokio::process::Command::new(&server);
    child
        .args([
            "-m",
            &gguf.to_string_lossy(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "-c",
            "16384",
            "-t",
            &threads.to_string(),
            "-ngl",
            "0",
            "--jinja",
        ])
        .env("LD_LIBRARY_PATH", server.parent().unwrap_or(data_dir))
        .current_dir(server.parent().unwrap_or(data_dir))
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        child.process_group(0);
    }
    let mut child = child.spawn().context("spawn llama-server")?;
    let pid = child.id().unwrap_or(0);
    let base = format!("http://127.0.0.1:{port}");
    let health = wait_health(&base, cancel).await;
    if health.is_err() {
        signal_group(pid, "-KILL");
        let _ = child.kill().await;
        return health.map(|_| unreachable!());
    }
    let result = chat(&base, messages, cancel).await;
    stop_server(pid, &mut child).await;
    let _ = lock;
    result
}

async fn wait_health(base: &str, cancel: &AtomicBool) -> Result<()> {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + HEALTH_WAIT;
    while tokio::time::Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) {
            bail!("inference cancelled");
        }
        if let Ok(response) = client.get(format!("{base}/health")).send().await {
            if response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                if body.contains("ok") {
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("llama-server did not become healthy")
}

pub async fn chat(base: &str, messages: &[ChatMessage], cancel: &AtomicBool) -> Result<Completion> {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA)?;
    let payload = json!({
        "messages": messages.iter().map(|message| json!({
            "role": message.role,
            "content": message.content,
        })).collect::<Vec<_>>(),
        "temperature": 0.2,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "patch",
                "strict": true,
                "schema": schema,
            }
        }
    });
    let client = reqwest::Client::new();
    let request = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&payload)
        .timeout(COMPLETE_TIMEOUT)
        .send();
    let response = tokio::select! {
        response = request => response.context("llama-server request")?,
        _ = wait_cancel(cancel) => bail!("inference cancelled"),
    };
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("llama-server returned an error: {}", tail_str(&text, 2000));
    }
    let body: serde_json::Value = tokio::select! {
        body = response.json() => body.context("parse llama-server response")?,
        _ = wait_cancel(cancel) => bail!("inference cancelled"),
    };
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .context("completion has no content")?;
    parse_completion(content)
}

pub fn parse_completion(content: &str) -> Result<Completion> {
    #[derive(Deserialize)]
    struct Patch {
        summary: String,
        diff: String,
    }
    let patch: Patch =
        serde_json::from_str(content.trim()).context("model output was not the patch schema")?;
    if patch.summary.trim().is_empty() || patch.diff.trim().is_empty() {
        bail!("model output was empty");
    }
    Ok(Completion {
        summary: patch.summary,
        diff: patch.diff,
    })
}

async fn wait_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn stop_server(pid: u32, child: &mut tokio::process::Child) {
    signal_group(pid, "-TERM");
    let grace = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    if grace.is_err() {
        signal_group(pid, "-KILL");
        let _ = child.kill().await;
    }
}

pub fn server_args(gguf: &Path, port: u16, threads: usize) -> Vec<String> {
    vec![
        "-m".into(),
        gguf.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "-c".into(),
        "16384".into(),
        "-t".into(),
        threads.to_string(),
        "-ngl".into(),
        "0".into(),
        "--jinja".into(),
    ]
}

pub async fn pull(data_dir: &Path) -> Result<()> {
    let lock = ModelLock::load()?;
    let free = free_bytes(data_dir)?;
    if free < 8 * 1024 * 1024 * 1024 {
        bail!("need at least 8 GB free under {}", data_dir.display());
    }
    std::fs::create_dir_all(lockfile::model_dir(data_dir))?;
    std::fs::create_dir_all(data_dir.join("models"))?;
    download_verified(
        &lock.llama_cpp_url,
        &lockfile::archive_path(data_dir),
        &lock.llama_cpp_sha256,
    )
    .await?;
    extract_llama(
        &lockfile::archive_path(data_dir),
        &lockfile::model_dir(data_dir),
    )?;
    download_verified(
        &lock.gguf_url,
        &lockfile::gguf_path(data_dir),
        &lock.gguf_sha256,
    )
    .await?;
    lockfile::verify_model(data_dir)?;
    Ok(())
}

pub async fn download_verified(url: &str, dest: &Path, expected: &str) -> Result<()> {
    if dest.is_file() && sha256_file(dest).ok().as_deref() == Some(expected) {
        lockfile::write_stamp(dest, expected)?;
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = dest.with_extension("partial");
    let mut last = None;
    for _ in 0..DOWNLOAD_ATTEMPTS {
        match download_once(url, &partial).await {
            Ok(()) => {
                let got = sha256_file(&partial)?;
                if got == expected {
                    std::fs::rename(&partial, dest)?;
                    lockfile::write_stamp(dest, expected)?;
                    return Ok(());
                }
                let _ = std::fs::remove_file(&partial);
                last = Some(anyhow::anyhow!("checksum mismatch: got {got}"));
            }
            Err(err) => {
                let _ = std::fs::remove_file(&partial);
                last = Some(err);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("download failed")))
}

async fn download_once(url: &str, partial: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;
    let mut request = client.get(url);
    let mut append = false;
    if partial.is_file() {
        let len = std::fs::metadata(partial)?.len();
        if len > 0 {
            request = request.header("Range", format!("bytes={len}-"));
            append = true;
        }
    }
    let response = request.send().await.context("download")?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let _ = std::fs::remove_file(partial);
        bail!("range not satisfiable");
    }
    if append && status != reqwest::StatusCode::PARTIAL_CONTENT && status != reqwest::StatusCode::OK
    {
        bail!("download status {status}");
    }
    if !append && !status.is_success() {
        bail!("download status {status}");
    }
    if status == reqwest::StatusCode::OK {
        append = false;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(partial)
        .await?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    Ok(())
}

fn extract_llama(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let mut components = path.components();
        components.next();
        let relative: PathBuf = components.collect();
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let out = dest.join(relative);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&out)?;
        }
    }
    let server = dest.join("llama-server");
    if !server.is_file() {
        bail!("archive did not contain llama-server");
    }
    Ok(())
}

pub fn mem_available() -> Result<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").context("read meminfo")?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .context("MemAvailable")?
                .parse()?;
            return Ok(kb * 1024);
        }
    }
    bail!("MemAvailable is missing")
}

pub fn free_bytes(path: &Path) -> Result<u64> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    let output = std::process::Command::new("df")
        .args(["-B1", "-P", &path.display().to_string()])
        .output()
        .context("df")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1).context("df produced no data")?;
    let avail = line
        .split_whitespace()
        .nth(3)
        .context("df available column")?;
    Ok(avail.parse()?)
}

pub fn physical_cores() -> usize {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return fallback_cores();
    };
    let mut pairs = std::collections::HashSet::new();
    let mut physical = None;
    let mut core = None;
    for line in text.lines() {
        if line.is_empty() {
            if let (Some(physical), Some(core)) = (physical.take(), core.take()) {
                pairs.insert((physical, core));
            }
        } else if let Some(rest) = line.strip_prefix("physical id") {
            physical = rest.split(':').nth(1).map(|value| value.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("core id") {
            core = rest.split(':').nth(1).map(|value| value.trim().to_string());
        }
    }
    if let (Some(physical), Some(core)) = (physical, core) {
        pairs.insert((physical, core));
    }
    if pairs.is_empty() {
        fallback_cores()
    } else {
        pairs.len()
    }
}

fn fallback_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn ephemeral_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::HotFunction;

    fn hot() -> HotFunction {
        HotFunction {
            name: "encode".into(),
            file: "src/lib.rs".into(),
            line_start: 1,
            line_end: 6,
            exclusive: 80_000,
            share: 0.8,
            source: "fn encode(input: &[u8]) -> Vec<u8> { input.to_vec() }".into(),
            is_test: false,
        }
    }

    #[test]
    fn prompt_names_the_test_and_forbids_new_dependencies() {
        let messages = build_messages(&PromptInput {
            test_name: "encode_batch",
            test_source: "fn encode_batch() { assert_eq!(1, 1); }",
            hot: &hot(),
            callee_sources: &[],
            repair_diff: None,
            repair_output: None,
        });
        let system = &messages[0].content;
        assert!(system.contains("unified diff"));
        assert!(system.contains("Do not add dependencies"));
        assert!(messages[1].content.contains("encode_batch"));
        assert!(messages[1].content.contains("fn encode"));
    }

    #[test]
    fn server_args_keep_the_gpu_out() {
        let args = server_args(Path::new("model.gguf"), 9, 3);
        let joined = args.join(" ");
        assert!(joined.contains("-ngl 0"));
        assert!(joined.contains("-c 16384"));
        assert!(joined.contains("--host 127.0.0.1"));
    }

    #[test]
    fn parses_a_schema_object() {
        let completion =
            parse_completion(r#"{"summary":"shorter","diff":"diff --git a/a b/a\n"}"#).unwrap();
        assert_eq!(completion.summary, "shorter");
    }

    #[test]
    fn extra_text_is_rejected() {
        assert!(parse_completion("sure, here is a diff").is_err());
    }

    #[tokio::test]
    async fn download_keeps_a_matching_file_and_drops_a_mismatch() {
        let good = b"noctum-bytes";
        let expected = crate::hashutil::sha256_bytes(good);
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(good))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        download_verified(&server.uri(), &dest, &expected)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), good);

        let bad = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(b"nope"))
            .expect(3)
            .mount(&bad)
            .await;
        let dest = dir.path().join("bad.bin");
        assert!(download_verified(&bad.uri(), &dest, &expected)
            .await
            .is_err());
        assert!(!dest.exists());
    }
}
