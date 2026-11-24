# Noctum

[![CI](https://github.com/SeanCheatham/Noctum/workflows/CI/badge.svg)](https://github.com/SeanCheatham/Noctum/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Noctum is a local overnight daemon for Rust repositories. While the machine is idle it builds a test as a static musl binary, counts the guest instructions that test runs, and asks a local model for a smaller version. With the default CPU goal, a change is kept only when repeated counts agree, the test still passes, and the instruction count drops by at least the configured floor. Memory and balanced goals can also keep measured allocation reductions within configured limits. Rejected changes are reset.

The CPU score is the guest instruction count. Memory goals use successful Rust allocation requests or requested bytes. Wall-clock time is recorded for the dashboard and is never used to accept or reject.

Version 0.2.0 replaces the earlier mutation-testing product. The old database is left unopened. There is no compatibility flag.

## What a night does

From 22:00 to 06:00, one pass per repository:

1. Check out `noctum/perf` in a worktree under the data directory. The repository you edit is not the git directory Noctum commits into, and Noctum never pushes.
2. Build the selected tests with `cargo test --release` for the selected Linux musl target.
3. Measure each eligible test twice in the pinned QEMU guest. The count includes instructions between two ARM64 `movz xzr` markers.
4. Rank the hot functions and ask the local model for a unified diff.
5. Apply the diff, run the package test suite, measure again, and either commit on `noctum/perf` or reset.

`noctum run` and the dashboard "run now" button ignore the window and the one-pass rule.

For the CPU goal, a test is eligible when two runs agree and the count is at least `min_insn_delta` (default 10,000). A candidate is kept when it is also at least `min_improvement_ratio` (default 1%) cheaper. A diff that drops or rewrites an assertion, lowers a fixed loop bound, or fails the suite is rejected. The model gets one repair if the build or the host suite fails, and no repair if the diff itself is invalid. Before a repair request, Noctum restores the original source. The request includes that source, the failed diff, and the test output. The replacement diff must apply to the original source.

## Prerequisites

Noctum requires an Apple silicon Mac with macOS 26 or later. The dashboard, database, Git operations, and local model run on macOS. A Swift helper uses Apple's Containerization framework to control a private Linux worker VM. No separate container service is required.

The worker builds ARM64 Linux musl binaries and runs the package test suite. QEMU counts the guest instructions with TCG. These counts do not measure macOS execution time. A project must support the Linux target for the selected features and workload. Tests that require macOS APIs cannot run in this guest.

Requirements:

- At least 16 GB of memory for the default model.
- `git` on the Mac. The worker supplies Rust and Linux build tools.
- At least 12 GiB of free disk space before the first worker build, plus space for the model and project build outputs. The default model file needs 9.14 GB.
- Network access for the first asset setup and public Cargo dependencies. Model inference is local.

Noctum checks free disk space during setup and guest operations. It stops the operation at a 2 GiB reserve. It checks both the data disk and the root disk during guest operations.

The default model is Inkling 2 Mini UD-Q2_K_XL. Its GGUF file is 9.14 GB. Noctum uses the matching Unsloth llama.cpp build pinned in `model.lock.toml`. Each supported host has a pinned server package. Noctum starts one server slot for one completion and stops it before QEMU runs. The context is 16,384 tokens. Reasoning effort is `low`. Noctum sets a 2,048 token reasoning budget within the 8,192 token response limit.

Set `quant = "q4"` in the `[model]` section of the application configuration to use UD-Q4_K_M on a host with more memory. Its GGUF file is 16.61 GB. Run `noctum model pull` after changing the quant. Q2 and Q4 use separate model files. Both use the pinned server. The resource checks apply to the selected quant.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
```

To build from source, install Rust and a Swift 6.4 toolchain with the macOS SDK. Then run:

```bash
sh scripts/build-host.sh release
cargo build --release --locked --bin noctum
```

Keep `target/release/noctum` and `target/release/noctum-host` in the same installation directory. The helper has the virtualization entitlement. `cargo install` alone does not install the helper.

The release archive contains both host binaries. The installer checks the archive checksum and the helper signature.

Developers can build and install both host tools from the current checkout:

```bash
./install_local.sh
```

The script installs into `${CARGO_HOME:-$HOME/.cargo}/bin`, usually `~/.cargo/bin`.
Use `--debug` for a debug build. Use `--bin-dir PATH` to select another installation directory.
The script reports when that directory is absent from `PATH`.

## First run

```bash
noctum model pull
noctum runtime prepare
noctum
```

The first worker setup downloads the fixed kernel and base image, then builds the measurement tools in a private VM. This can take several minutes. A source checkout is not required. The CLI contains the worker source inputs. The completed build log is saved as `runtime/build.log` in the data directory. Helper errors are saved as `runtime/helper.log`.

Later runs use the prepared worker disk. Noctum starts a fresh VM before each build and measurement operation so shared files have their current contents. It stops the worker VM before each model request. Cargo downloads, toolchains, and compiler outputs remain on disk between starts.

With no subcommand, Noctum starts the daemon and the dashboard at <http://127.0.0.1:8420>. The dashboard checks the Host header and has no password. Do not bind it to another interface.

In a Rust repository, run:

```bash
noctum init
noctum doctor
```

`init` finds the Git root, creates a small `noctum.toml`, and registers the repository. It uses the root package or a single `default-members` package when available. Use `noctum init --package NAME` to select another package. The command lists package names when a selection is necessary. It reads declared members and local path dependencies, including inherited workspace dependencies. Host Rust tools are not necessary for this step.

An existing `noctum.toml` is kept. An existing registration is kept, including its enabled state. `init` does not make a Git commit. Commit your source changes before a run. Noctum uses the source at `HEAD`.

You can also add a repository from the dashboard. The repository needs a `noctum.toml` owned by you. The file must not be world-writable. Unknown keys cause an error, including keys from the old mutation configuration.

```bash
noctum run --repo /path/to/repo   # one pass, ignoring the window
noctum run --test encode_batch --max-attempts 1
noctum run --all
noctum measure --test encode_batch
noctum review --repo /path/to/repo
noctum model status
noctum status
```

`init`, `doctor`, and `review` use the current repository by default. Use `--repo PATH` to select another repository. Use the global `--config PATH` option to select the same application configuration for each command.

`doctor` checks Git state, repository configuration, package selection, registration, model hashes, the Swift helper, and worker assets. It reports failures together and gives correction instructions. It does not download files, build a worker, or register a repository. A successful setup check does not establish that a test can build or run in the guest.

`review` shows the latest run for the selected repository. Accepted changes come first, with saved diffs, instruction counts, test check results, the target, and the run base commit. Each available commit has a `git cherry-pick` command. Changes can depend on earlier commits on `noctum/perf`. Apply them in commit order. A pending commit or a missing Git object has no apply command. Rejected trials and baseline test results follow the accepted changes. The command does not apply changes. Instruction reductions do not establish elapsed-time improvements.

## Manual runs and measurements

`run` uses the current Git repository by default. It must be registered and enabled. Use `--repo PATH` to select another repository. Use `--all` to run all enabled repositories. `run` uses committed source and ignores the schedule window.

Model errors are printed in the command report and give a nonzero exit status. A later run can retry a function after a model error.

Noctum usually skips a function if its source is unchanged after a completed trial. Use `run --retry` to try it again. This option keeps the trial history and all patch, test, and instruction count checks. Use it after a model or build configuration change if the source has not changed.

`run --test NAME` selects one test for this run. It replaces the configured test list and does not select benches. The test name must match exactly, or match the end of a full module path. It is not a substring filter. To select a harness, use a name such as `tests/parse.rs::tests::encode_batch` or `'unittests src/lib.rs::tests::encode_batch'`. Noctum first builds the required harnesses and reads their active test lists. Tests disabled by target or feature configuration are excluded. A selected library test does not build unrelated integration test targets. The full package test suite still checks proposed patches. Ambiguous names produce a skip message with full test names. The `skip_tests` rules and guest checks still apply. The repository configuration file is kept.

External test modules can use `file.rs`, `directory/mod.rs`, inline modules, and `#[path]` attributes. Noctum keeps the full harness test name and the function path within each file. Generated tests without a directly editable source function are reported as skipped. For these tests, add a plain `#[test]` wrapper around the workload. Custom test harnesses are not used as libtest harnesses.

`--max-attempts N` limits model requests for the command. Repair requests count toward the limit. If a failed patch needs a repair after the limit is reached, Noctum records the failed trial and restores its worktree. The configured candidate limit also applies. The request limit must be at least one. `--test` cannot be used with `--all`.

`measure` builds and measures the current working source in a separate detached worktree. The source includes staged changes, unstaged changes, and new files that Git does not ignore. Ignored files are excluded. The original branch, index, and working files are kept. Git stores the source snapshot as a commit object without creating a branch. The measurement worktree is removed after the run.

Measurement uses the same static musl build, fence markers, guest target, and two-run count and output checks as an optimization run. Both output streams must match. Noctum excludes only the libtest duration from the standard output comparison. It does not request model completions or run the full test suite check. It does not need model files or repository registration. It needs a valid `noctum.toml` and the measurement runtime. Use `--test NAME` to select one test. Otherwise, the repository test list and scan limits apply. Counts below the optimization floor are reported and can be used for comparison.

Measurement reports are saved under `measurements/RUN_NAME/` in the data directory. `report.txt` contains the results, and `results.db` contains the measurement history. These reports are separate from optimization history. The data directory must be outside the source repository. Compare counts only for the same target and test workload.

Both commands show the current step, elapsed time, and log path. Logs are saved under `logs/` in the data directory. The commands use the same run lock as the daemon. Control-C requests a stop and waits for cleanup. A build failure, missing test, failed repeat check, or run with no stable count gives a nonzero exit status. Skip messages include instructions for fixed workloads, clocks, network access, test attributes, and bench configuration.

## Function sweeps and memory goals

Set the search goal and selection method in the application configuration:

```toml
[optimization]
goal = "balanced"                 # cpu, memory, or balanced
selection = "sweep"               # hot or sweep
memory_metric = "allocated_bytes" # allocated_bytes or allocations
min_memory_delta = 1024           # units of the selected memory metric
min_memory_ratio = 0.01
max_cpu_increase_ratio = 0.0
max_memory_increase_ratio = 0.0
```

The default goal is `cpu`, and the default selection is `hot`. Existing CPU builds do not use the measurement allocator. The default memory floor is one unit and one percent. Use a larger `min_memory_delta` to require a larger reduction. Ratios in this section must be from zero to one.

`hot` selects functions with at least one percent of the measured instructions. `sweep` includes all functions that the measured guest addresses map to source, including those below one percent. Sweep selection excludes test functions. Memory goals also include functions below one percent and exclude test functions. Compiler inlining and debug information can limit source attribution. A function without mapped execution is not selected for a model request.

The sweep uses the existing test, scan, candidate, and model request limits. It starts with functions that consume the most measured instructions. Memory goals first rank functions by the number of possible allocation, copy, and growth sites. Each selected function gets one proposal and, if the build or suite fails, at most one repair. Non-test patches must change only that function body. The signature, attributes, other functions, and test workloads must stay unchanged. Added unsafe blocks are rejected.

Completed trials suppress another request under the same search conditions. The search identity includes the function file, selected workload and harness, measured binary hash, repository configuration, model lock, goal, limits, and measurement profile. Changes to these inputs permit another trial. Model errors remain eligible for retry. Use `run --retry` to bypass completed trial history.

Before each model request, Noctum checks the function syntax for possible allocations, copies, buffer growth, searches inside loops, loop depth, named calls, and effects such as locks and unsafe blocks. The request includes these findings and available same-file callee source. Findings do not resolve types or call targets. They do not prove that an allocation occurs or that a change will help. No additional analyzer installation is required.

Inspect the function inventory without a worker, model, Cargo build, or repository registration:

```bash
noctum analyze --repo /path/to/repo
noctum analyze --repo /path/to/repo --json
```

This command reads current tracked and non-ignored Rust files. It includes protected functions in the report and marks them as protected. It does not change source or create run history. The inventory includes functions without measurement workloads; these functions need representative workloads before optimization.

### Memory measurements and acceptance

The `memory` and `balanced` goals enable a measurement allocator in the fence crate. The allocator delegates to Rust's system allocator. It counts successful allocation and zeroed-allocation requests inside the fence, plus successful reallocation requests. A reallocation counts as one request and its full new requested size. Deallocation does not reduce total allocated bytes. Counters cover the measured process, including its Rust threads. Allocations in other processes or through separate native allocators are excluded. Libtest can allocate on another thread during this window. A fixed `harness = false` benchmark can isolate a single-threaded workload more closely.

These values are allocation traffic. They are not peak heap size, retained memory, resident memory, or GPU memory. Existing custom global allocators are not supported. Source attributes are checked before a model request; an allocator supplied through a dependency or macro can instead cause a measurement build error.

Both memory counters and the instruction count must repeat exactly. A missing or invalid allocator record fails the run checks. Allocator records are removed from measurement error output before output comparison. Application output bytes are kept. CPU-only runs keep all output bytes without allocator record processing.

The `memory` goal requires the selected memory metric to decrease by both memory floors. The `balanced` goal accepts either that memory reduction or a CPU reduction that meets both CPU floors. Both goals enforce the permitted instruction increase and the permitted increase in each memory counter. The default permits no increase. A zero baseline permits no increase even when a ratio limit is set.

Memory instrumentation adds instructions. Its instruction counts have a separate allocator profile and must be compared only with the same instrumentation. The package suite checks the patch without the injected fence dependency before candidate measurement. These checks do not establish native elapsed-time improvements.

Run reports, `review`, and the dashboard show memory counts where available. Trials also save source findings. Old history remains readable; old rows have no memory measurements. The dashboard Settings page shows the optimization configuration. Edit the application file and restart the daemon to change it.

## Configuration

Copy `config.example.toml` to `~/Library/Application Support/com.noctum.noctum/config.toml`. Use `--config` to select another file. The schedule, the keep floor, the web bind, and the guest memory live there. The model quant is selected in `[model]`. Model files and server packages are fixed in `model.lock.toml`. Worker assets are fixed in `runtime/assets.lock.toml`. The guest build script checks source archive hashes.

`perf.db` is created in the data directory. An older `noctum.db` is not opened and is not deleted.

## Repository file

```toml
package = "my-crate"
tests = []            # empty: discover tests, up to max_scan
skip_tests = []
benches = []
features = []
all_features = false
no_default_features = false
protected_paths = []
timeout_seconds = 600
clean_paths = ["target"]
```

`clean_paths` selects files and directories to remove before a build. Each path must be inside the worktree. A path cannot select `.git` or pass through a symbolic link. If the selected path is a symbolic link, only the link is removed.

`panic = "abort"` is refused, because the fence has to run on unwind. A dynamic binary (`PT_INTERP`) is skipped. Tests that open a socket, read the clock, or otherwise match the denylist are skipped and the reason is stored.

A listed bench that uses Criterion, `#[bench]`, or the default harness is stored as skipped. A listed bench with `harness = false` and a `fn main` is measured: the fence wraps `main`, and the guest runs that binary with no libtest filter. The loop inside `main` has to be a fixed count.

`features`, `all_features`, and `no_default_features` apply to measurement builds and package test suite checks. Use `no_default_features = true` when a default feature requires a service or a platform library that the worker cannot provide. Required features for the selected test must still be enabled.

The worker uses counter version `ram-insn-v2`. It counts one QEMU callback before each guest instruction between the markers, including kernel instructions. A faulting instruction can count again when it is tried again. These counts are not hardware retirement counts. Each measurement starts a fresh guest with seed `1`, a fixed RTC date, and the VM clock. The guest has no host filesystem devices.

The guest gets tracked and non-ignored source files and the selected test binary in a RAM archive. Source files are read-only. Tests use the package working directory. Absolute source paths from `CARGO_MANIFEST_DIR` point to the guest source files. Links outside the repository are refused. Input size is limited to one quarter of `guest.memory_mb`. Combined output size is limited to 64 MiB. Output keeps all bytes.

Run reports record the counter version, CPU model, guest memory, and guest manifest hash. Old bundles use `legacy-tb-v1`. History from another counter profile does not suppress a new trial or a scheduled run. Keep counts from different profiles separate.

## Project compatibility checks

Run the pinned upstream project checks with:

```bash
sh scripts/build-host.sh debug
cargo build --locked --bin noctum
python3 tests/compatibility/run.py --output /path/to/new-output-directory
```

The script fetches the exact revisions in `tests/compatibility/projects.json`. It runs selected upstream tests twice through the production CLI. For supported configurations, each command also checks two guest runs. Counts must agree between commands. Known build failures must report the expected compiler error. Source files, HEAD, the index, and branch references must be kept. Measurement must not prepare model files or create model trials. Logs, database records, and `report.json` are saved in the output directory. The measurement runtime must be available. Use `--project NAME` to select one project.

Use `--data-dir PATH` to reuse a measurement data directory and its worker caches. The base64 and memchr cases must also stay above the optimization floor. The SDK runtime results are in `experiments/macos-containerization/runtime-results.json`.

These checks establish support for the listed tests and feature sets. They do not establish support for every test in each project. Small tests can be reduced to a few instructions by the compiler. Such tests need a fixed workload with runtime input before they can provide a useful optimization target.

Tests that use `thread_rng`, `OsRng`, or common entropy constructors are skipped before guest execution. A fixed seed is allowed. Indirect calls can still use random input or external state, so the repeat checks remain necessary. Counts from large allocation-heavy workloads can also disagree. These tests remain ineligible until the counts repeat exactly.

## Dashboard

- Home: phase (idle, building, checking, measuring, inferring), window, bundle and model status
- Repository: last run, baselines, and each candidate's hot function, fenced counts before and after, whether it was kept, and why
- Trial: diff, those counts, the gate that failed, tool output
- Settings: schedule, thresholds, web bind, and the lockfile facts (read-only)

## Licenses

The Rust crates are MIT OR Apache-2.0. `qemu/plugin/noctum_profile.c` is GPL-2.0-or-later because it is built against the QEMU plugin API. It is not linked into the Noctum binary. The guest runner and init script are MIT OR Apache-2.0.

## Build and check the worker

`noctum runtime prepare` uses the Swift helper to download the base image and create a private EXT4 disk. `runtime/build-worker.sh` runs inside that VM. It builds QEMU 9.2.0, Linux 6.12.10, BusyBox 1.36.1, the profiling plugin, and the Rust worker. Source archive hashes are checked before each build. A source change causes Noctum to prepare the worker again.

Run the unit tests and code checks with:

```bash
cargo test --workspace --all-features --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

The live worker tests need a prepared data directory. Set `NOCTUM_TEST_RUNTIME_DATA` to that directory. Set `NOCTUM_HOST_HELPER` to the absolute path of the helper. Then run:

```bash
cargo test --locked --test macos_worker -- --ignored --nocapture --test-threads=1
```

Run the complete memory sweep check with the same environment:

```bash
cargo test --locked --test macos_optimization -- --ignored --nocapture --test-threads=1
```

This test uses a fixed model response. It checks allocation reduction, acceptance, saved results, and preservation of the original repository. It does not need model files.

These tests check repeated counts, raw output bytes, changed source files, and shutdown after the controller exits. `tests/host_helper.py` checks the Swift helper alone with a real VM.

## Mac worker lifecycle

Each pass owns one worker VM. The worker can write to the worktree, measurement, and scratch directories. Repository inputs, Git metadata, and the fence source are shared read-only. Model files and `perf.db` are outside these shares. Guest commands use the Mac user ID.

The worker VM and the local model run in sequence. The worker VM has two virtual CPUs. Each QEMU measurement has one virtual CPU and no network device. Measurement files remain on the private worker disk. Logs and results are copied to the data directory.

Control-C stops the pass and its worker. If the controller exits, its pipe closes and the Swift helper stops the VM. A file lock prevents two Noctum processes from using the same worker disk. Failed or cancelled operations restore the Noctum worktree to its current commit.

Public Cargo dependencies need no host credentials. Private dependencies need credentials configured in the worker environment.

To check worker and model process separation during a real run:

```bash
python3 tests/runtime_phases.py --output /path/to/new-results --repo /path/to/repo -- \
  /path/to/noctum --config /path/to/config.toml run --repo /path/to/repo --max-attempts 1 --retry
```

This check needs a prepared model and worker. It records process changes, tests for overlap, and checks the original source after the run.
