//! Lockfiles compiled into the binary. A night refuses a component whose
//! on-disk bytes disagree with these hashes.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::hashutil::sha256_file;

const MODEL_TOML: &str = include_str!("../model.lock.toml");
const QEMU_TOML: &str = include_str!("../qemu.lock.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLock {
    pub llama_cpp_tag: String,
    pub llama_cpp_url: String,
    pub llama_cpp_sha256: String,
    pub gguf_url: String,
    pub gguf_filename: String,
    pub gguf_sha256: String,
    pub gguf_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuLock {
    pub id: String,
    pub url: String,
    pub sha256: String,
}

impl ModelLock {
    pub fn load() -> Result<Self> {
        toml::from_str(MODEL_TOML).context("parse model.lock.toml")
    }
}

impl QemuLock {
    pub fn load() -> Result<Self> {
        toml::from_str(QEMU_TOML).context("parse qemu.lock.toml")
    }

    pub fn published(&self) -> bool {
        !self.url.is_empty() && !self.sha256.is_empty()
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
    data_dir.join("llama.cpp").join(lock.llama_cpp_tag)
}

pub fn archive_path(data_dir: &Path) -> std::path::PathBuf {
    model_dir(data_dir).join("archive.tar.gz")
}

pub fn server_path(data_dir: &Path) -> std::path::PathBuf {
    model_dir(data_dir).join("llama-server")
}

pub fn gguf_path(data_dir: &Path) -> std::path::PathBuf {
    let lock = ModelLock::load().expect("model lockfile");
    data_dir.join("models").join(lock.gguf_filename)
}

pub fn qemu_dir(data_dir: &Path) -> std::path::PathBuf {
    let lock = QemuLock::load().expect("qemu lockfile");
    data_dir.join("qemu").join(lock.id)
}

pub fn bundle_archive(data_dir: &Path) -> std::path::PathBuf {
    qemu_dir(data_dir).join("bundle.tar.gz")
}

/// Full hash check used before a night and by `noctum model status`.
pub fn verify_model(data_dir: &Path) -> Result<()> {
    let lock = ModelLock::load()?;
    expect_hash(
        &archive_path(data_dir),
        &lock.llama_cpp_sha256,
        "llama.cpp archive",
    )?;
    if !server_path(data_dir).is_file() {
        bail!("llama-server is not extracted");
    }
    let gguf = gguf_path(data_dir);
    expect_hash(&gguf, &lock.gguf_sha256, "model")?;
    let len = std::fs::metadata(&gguf)?.len();
    if len != lock.gguf_bytes {
        bail!("model length is {len}, lockfile says {}", lock.gguf_bytes);
    }
    Ok(())
}

pub fn verify_qemu(data_dir: &Path) -> Result<()> {
    let lock = QemuLock::load()?;
    if !lock.published() {
        bail!(
            "QEMU bundle {} has no published url and sha256. Build it with qemu/build-bundle.sh and record the tarball hash in qemu.lock.toml",
            lock.id
        );
    }
    expect_hash(&bundle_archive(data_dir), &lock.sha256, "QEMU bundle")?;
    for name in [
        "qemu-system-x86_64",
        "noctum_profile.so",
        "vmlinuz",
        "initramfs.cpio.gz",
    ] {
        let path = qemu_dir(data_dir).join(name);
        if !path.is_file() {
            bail!("QEMU bundle is missing {}", path.display());
        }
    }
    Ok(())
}

/// Dashboard check. It compares the stamp written after a verified download
/// and the file length. The night hashes the bytes again.
pub fn model_stamp(data_dir: &Path) -> AssetState {
    let Ok(lock) = ModelLock::load() else {
        return AssetState::Mismatch;
    };
    stamp_state(
        &gguf_path(data_dir),
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

pub fn qemu_stamp(data_dir: &Path) -> AssetState {
    let Ok(lock) = QemuLock::load() else {
        return AssetState::Mismatch;
    };
    if !lock.published() {
        return AssetState::Missing;
    }
    stamp_state(&bundle_archive(data_dir), &lock.sha256, None)
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
    fn model_lock_pins_the_coder_quant() {
        let lock = ModelLock::load().unwrap();
        assert_eq!(lock.llama_cpp_tag, "b12528");
        assert!(lock.gguf_filename.contains("q5_k_m"));
        assert_eq!(lock.gguf_sha256.len(), 64);
        assert_eq!(lock.llama_cpp_sha256.len(), 64);
        assert!(lock.gguf_bytes > 5_000_000_000);
    }

    #[test]
    fn qemu_bundle_is_not_published_yet() {
        let lock = QemuLock::load().unwrap();
        assert_eq!(lock.id, "qemu-9.2.0-linux-6.12.10");
        assert!(!lock.published());
        let dir = tempfile::tempdir().unwrap();
        let error = verify_qemu(dir.path()).unwrap_err().to_string();
        assert!(error.contains("qemu/build-bundle.sh"), "{error}");
    }
}
