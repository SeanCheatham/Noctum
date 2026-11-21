# Noctum

[![CI](https://github.com/SeanCheatham/Noctum/workflows/CI/badge.svg)](https://github.com/SeanCheatham/Noctum/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Noctum is a local overnight daemon for Rust repositories. While the machine is idle it builds a test as a static musl binary, counts the guest instructions that test runs, and asks a local model for a smaller version. A change is kept only when the same count comes back twice, the test still passes, and the instruction count drops by at least the configured floor. Anything else is reset.

The score is that instruction count. Wall-clock time is recorded for the dashboard and is never used to accept or reject.

Version 0.2.0 replaces the earlier mutation-testing product. The old database is left unopened. There is no compatibility flag.

## What a night does

From 22:00 to 06:00, one pass per repository:

1. Check out `noctum/perf` in a worktree under the data directory. The repository you edit is not the git directory Noctum commits into, and Noctum never pushes.
2. Build the selected tests with `cargo test --release` for `x86_64-unknown-linux-musl`.
3. Measure each eligible test twice in the pinned QEMU guest. The count is the instructions between two `out 0xe9` fences.
4. Rank the hot functions and ask the local model for a unified diff.
5. Apply the diff, run the host test suite, measure again, and either commit on `noctum/perf` or reset.

`noctum run` and the dashboard "run now" button ignore the window and the one-pass rule.

A test is eligible when two runs agree and the count is at least `min_insn_delta` (default 10,000). A candidate is kept when it is also at least `min_improvement_ratio` (default 1%) cheaper. A diff that drops an assertion, lowers a fixed loop bound, or fails the suite is rejected. The model gets one repair if the build or the host suite fails, and no repair if the diff itself is invalid.

## Prerequisites

Linux x86_64.

- Rustup, with the `x86_64-unknown-linux-musl` target already installed. Noctum does not run `rustup target add`.
- `git` and `cargo`.
- About 8 GB free for the model download, and enough RAM for the GGUF plus 1 GB before inference starts. The guest is not running while the model is.

The model is Qwen2.5-Coder-7B-Instruct Q5_K_M, run by the llama.cpp build pinned in `model.lock.toml`. Noctum starts that server for one completion and stops it before QEMU. CPU only. `qemu/real-pass.sh` is an ignored test of that same loop against a GGUF you already have; it does not change the pin.

The guest is the tarball named by `qemu.lock.toml`: a pinned QEMU, the TCG plugin, a pinned kernel, and an initramfs. A distro QEMU is never used. `qemu/build-bundle.sh` prints the tarball sha256. Record that hash in the lockfile. `noctum qemu fetch` downloads `url` when a release hosts the tarball. Until then, `noctum qemu install` checks a local tarball against the hash and unpacks it.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
```

Or build it:

```bash
cargo install --path . --locked
```

The published binary is Linux x86_64. The installer rejects other machines.

## First run

```bash
rustup target add x86_64-unknown-linux-musl
noctum model pull
noctum qemu fetch
noctum
```

With no subcommand, Noctum starts the daemon and the dashboard at <http://127.0.0.1:8420>. The dashboard checks the Host header and has no password. Do not bind it to another interface.

Add a repository from the dashboard. The repository needs a `noctum.toml` owned by you and not world-writable. See the example in this tree. Unknown keys are an error, including keys from the old mutation config.

```bash
noctum run --repo /path/to/repo   # one pass, ignoring the window
noctum model status
noctum status
```

## Configuration

Copy `config.example.toml` to `~/.config/noctum/config.toml`. The schedule, the keep floor, the web bind, and the guest memory live there. The model and the QEMU bundle live in the lockfiles.

`perf.db` is created in the data directory. An older `noctum.db` is not opened and is not deleted.

## Repository file

```toml
package = "my-crate"
tests = []            # empty: discover tests, up to max_scan
skip_tests = []
benches = []
protected_paths = []
timeout_seconds = 600
clean_paths = ["target"]
```

`panic = "abort"` is refused, because the fence has to run on unwind. A dynamic binary (`PT_INTERP`) is skipped. Tests that open a socket, read the clock, or otherwise match the denylist are skipped and the reason is stored.

A listed bench that uses Criterion, `#[bench]`, or the default harness is stored as skipped. A listed bench with `harness = false` and a `fn main` is measured: the fence wraps `main`, and the guest runs that binary with no libtest filter. The loop inside `main` has to be a fixed count.

## Dashboard

- Home: phase (idle, building, measuring, inferring), window, bundle and model status
- Repository: last run, baselines, and each candidate's hot function, fenced counts before and after, whether it was kept, and why
- Trial: diff, those counts, the gate that failed, tool output
- Settings: schedule, thresholds, web bind, and the lockfile facts (read-only)

## Licenses

The Rust crates are MIT OR Apache-2.0. `qemu/plugin/noctum_profile.c` is GPL-2.0-or-later because it is built against the QEMU plugin API. It is not linked into the Noctum binary. The guest runner and init script are MIT OR Apache-2.0.

## Building the guest

`qemu/build-bundle.sh` downloads QEMU 9.2.0, Linux 6.12.10, and BusyBox 1.36.1, checks their hashes, and stages one tarball. It prints the tarball hash. Put that hash in `qemu.lock.toml`. Put a release URL there when one hosts the tarball. The release workflow builds this bundle on a tag or a manual run and uploads it as an artifact; it does not rewrite the lockfile.

`qemu/smoke.sh` builds `qemu/fixtures/harness` as a static musl test and boots it twice in the guest. The harness calls `noctum_fence::guard()`. The command prints the instruction counts between the fences. `small_loop` and `encode_batch` must each repeat exactly, and the longer loop must count more. This is not part of `cargo test`.
