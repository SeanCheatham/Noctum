# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
