//! Lockfiles compiled into the binary. A night refuses a component whose
//! on-disk bytes disagree with these hashes.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::config::ModelQuant;
use crate::hashutil::sha256_file;

const MODEL_TOML: &str = include_str!("../model.lock.toml");

#[derive(Debug, Clone)]
pub struct ModelLock {
    pub llama_cpp_tag: String,
    pub llama_cpp_url: String,
    pub llama_cpp_sha256: String,
    pub gguf_url: String,
    pub gguf_filename: String,
    pub gguf_sha256: String,
    pub gguf_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    runtime: RuntimeFile,
    models: ModelsFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFile {
    tag: String,
    macos_aarch64: Archive,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    url: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsFile {
    q2: Weights,
    q4: Weights,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Weights {
    url: String,
    filename: String,
    sha256: String,
    bytes: u64,
}

impl ModelLock {
    pub fn load() -> Result<Self> {
        Self::load_quant(ModelQuant::default())
    }

    pub fn load_quant(quant: ModelQuant) -> Result<Self> {
        Self::for_platform(quant, std::env::consts::OS, std::env::consts::ARCH)
    }

    fn for_platform(quant: ModelQuant, os: &str, arch: &str) -> Result<Self> {
        let file: ModelFile = toml::from_str(MODEL_TOML).context("parse model.lock.toml")?;
        let archive = match (os, arch) {
            ("macos", "aarch64") => file.runtime.macos_aarch64,
            _ => bail!("model inference is not supported on {os} {arch}"),
        };
        let weights = match quant {
            ModelQuant::Q2 => file.models.q2,
            ModelQuant::Q4 => file.models.q4,
        };
        Ok(Self {
            llama_cpp_tag: file.runtime.tag,
            llama_cpp_url: archive.url,
            llama_cpp_sha256: archive.sha256,
            gguf_url: weights.url,
            gguf_filename: weights.filename,
            gguf_sha256: weights.sha256,
            gguf_bytes: weights.bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetState {
    Missing,
    Present,
    Mismatch,
    Unverified,
}

pub fn model_dir(data_dir: &Path) -> std::path::PathBuf {
    let lock = ModelLock::load().expect("model lockfile");
    data_dir
        .join("llama.cpp")
        .join(lock.llama_cpp_tag)
        .join(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
}

pub fn archive_path(data_dir: &Path) -> std::path::PathBuf {
    model_dir(data_dir).join("archive.tar.gz")
}

pub fn server_path(data_dir: &Path) -> std::path::PathBuf {
    model_dir(data_dir).join("llama-server")
}

pub fn gguf_path(data_dir: &Path, quant: ModelQuant) -> std::path::PathBuf {
    let lock = ModelLock::load_quant(quant).expect("model lockfile");
    data_dir.join("models").join(lock.gguf_filename)
}

/// Full hash check used before a night and by `noctum model status`.
pub fn verify_model(data_dir: &Path, quant: ModelQuant) -> Result<()> {
    let lock = ModelLock::load_quant(quant)?;
    expect_hash(
        &archive_path(data_dir),
        &lock.llama_cpp_sha256,
        "llama.cpp archive",
    )?;
    if !server_path(data_dir).is_file() {
        bail!("llama-server is not extracted");
    }
    let gguf = gguf_path(data_dir, quant);
    expect_hash(&gguf, &lock.gguf_sha256, "model")?;
    let len = std::fs::metadata(&gguf)?.len();
    if len != lock.gguf_bytes {
        bail!("model length is {len}, lockfile says {}", lock.gguf_bytes);
    }
    Ok(())
}

/// Dashboard check. It compares the stamp written after a verified download
/// and the file length. The night hashes the bytes again.
pub fn model_stamp(data_dir: &Path, quant: ModelQuant) -> AssetState {
    let Ok(lock) = ModelLock::load_quant(quant) else {
        return AssetState::Mismatch;
    };
    stamp_state(
        &gguf_path(data_dir, quant),
        &lock.gguf_sha256,
        Some(lock.gguf_bytes),
    )
}

pub fn llama_stamp(data_dir: &Path) -> AssetState {
    let Ok(lock) = ModelLock::load() else {
        return AssetState::Mismatch;
    };
    let archive = stamp_state(&archive_path(data_dir), &lock.llama_cpp_sha256, None);
    if archive != AssetState::Present {
        return archive;
    }
    if server_path(data_dir).is_file() {
        AssetState::Present
    } else {
        AssetState::Missing
    }
}

fn stamp_state(path: &Path, expected: &str, bytes: Option<u64>) -> AssetState {
    if !path.is_file() {
        return AssetState::Missing;
    }
    if let Some(bytes) = bytes {
        if std::fs::metadata(path).map(|meta| meta.len()).ok() != Some(bytes) {
            return AssetState::Mismatch;
        }
    }
    match std::fs::read_to_string(stamp_path(path)) {
        Ok(text) if text.trim() == expected => AssetState::Present,
        Ok(_) => AssetState::Mismatch,
        Err(_) => AssetState::Unverified,
    }
}

pub fn stamp_path(path: &Path) -> std::path::PathBuf {
    Path::new(&format!("{}.sha256", path.display())).to_path_buf()
}

pub fn write_stamp(path: &Path, sha: &str) -> Result<()> {
    std::fs::write(stamp_path(path), format!("{sha}\n")).context("write checksum stamp")
}

fn expect_hash(path: &Path, expected: &str, label: &str) -> Result<()> {
    if !path.is_file() {
        bail!("{label} is missing at {}", path.display());
    }
    let got = sha256_file(path)?;
    if got != expected {
        bail!("{label} sha256 is {got}, lockfile says {expected}");
    }
    write_stamp(path, &got)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_lock_selects_matching_packages_and_quant_files() {
        let lock = ModelLock::load().unwrap();
        assert_eq!(lock.llama_cpp_tag, "b12588-mix-c0c3de8");
        assert!(lock.gguf_filename.contains("UD-Q2_K_XL"));
        assert_eq!(lock.gguf_sha256.len(), 64);
        assert_eq!(lock.llama_cpp_sha256.len(), 64);
        assert_eq!(lock.gguf_bytes, 9_138_715_328);
        {
            let (os, arch, package) = ("macos", "aarch64", "macos-arm64");
            let small = ModelLock::for_platform(ModelQuant::Q2, os, arch).unwrap();
            let large = ModelLock::for_platform(ModelQuant::Q4, os, arch).unwrap();
            assert!(small.llama_cpp_url.contains(package));
            for lock in [&small, &large] {
                let revision = lock.gguf_url.split("/resolve/").nth(1).unwrap();
                let revision = revision.split('/').next().unwrap();
                assert_eq!(revision.len(), 40);
                assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
            }
            assert_eq!(small.llama_cpp_sha256, large.llama_cpp_sha256);
            assert_ne!(small.gguf_sha256, large.gguf_sha256);
            assert!(large.gguf_filename.contains("UD-Q4_K_M"));
            assert!(large.gguf_bytes > small.gguf_bytes);
            assert_ne!(
                gguf_path(Path::new("data"), ModelQuant::Q2),
                gguf_path(Path::new("data"), ModelQuant::Q4)
            );
        }
        assert!(ModelLock::for_platform(ModelQuant::Q2, "windows", "x86_64").is_err());
    }
}
