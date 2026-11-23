# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The host requires macOS 26 or later on Apple silicon.
- A signed Swift helper uses Apple Containerization to control the worker VM.
- The worker VM stops before local model inference. Cargo caches remain on disk.
- Docker, the container command adapter, and Linux host support are removed.
- `noctum runtime prepare` downloads fixed assets and builds the ARM64 guest tools.

### Fixed

- A diff that lowers a fixed `for` bound rejects that candidate and is not sampled again. A host-suite failure still gets its one repair.
- The real-pass harness does not draw another sample after a diff that drops or rewrites an assertion, or after a host failure whose diff deletes the nested scan the assertion observes. A rewrite that still walks the input can be sampled again.
- A diff that rewrites an assertion in the measured test is rejected, as is one that drops the assertion. The prompt asks for a cheaper algorithm that returns the same value, and no longer tells the model to delete a computation the return value ignores.
- After an accept, the next hot function in that file is still tried, and it is scored against a fresh baseline. A rebaseline that does not repeat ends the night's remaining candidates.

### Added

- A listed bench with `harness = false` and `fn main` is measured and can be kept. Criterion, `#[bench]`, and the default harness stay skipped
- The repository page lists each candidate's hot function, the fenced counts before and after the change, and whether the night kept it
- The real-pass fixture counts equal byte pairs with a nested scan. The tests assert that count, so deleting the scan fails the host suite. A linear count of the same pairs still passes. The script keeps a commit only when the measured edit clears the gate
- Libtest's `finished in` duration is left out of the stdout comparison, so the timer cannot reject a cheaper run
- `git apply --recount` accepts a model diff whose hunk counts are wrong when the changed lines still match
- The model prompt requires a `diff --git` header and context lines copied from the source. A hunk whose header line is wrong is still checked where those lines match
- The night's candidate is the hottest in-repo function in the QEMU guest profile. A measured test whose PCs do not map back to repository source stops the run instead of substituting a named function

## [0.2.0] - 2026-11-20

The mutation-testing product ended. Noctum is an overnight daemon that keeps a Rust change when a measured test gets cheaper.

### Added

- Overnight window (22:00–06:00) with one pass per repository, plus `noctum run` and a dashboard "run now" that ignore the window
- Static musl test binaries measured in a pinned QEMU guest
- Instruction-count scoring between `out 0xe9` fences
- A llama.cpp server that Noctum starts and stops itself
- Accept or reject onto the local `noctum/perf` branch, which is never pushed
- Localhost dashboard and a new `perf.db`
- `noctum model pull`, `noctum qemu fetch`, and `noctum status`

### Removed

- Mutation testing, architecture summaries, and diagram generation
- The previous local-model client, its endpoint list, and the old database
- macOS and Node.js support

## [0.1.0] - 2024-12-24

### Added

- Initial release of the previous analysis and mutation-testing product

[Unreleased]: https://github.com/SeanCheatham/Noctum/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/SeanCheatham/Noctum/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/SeanCheatham/Noctum/releases/tag/v0.1.0
