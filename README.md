# Noctum

[![CI](https://github.com/SeanCheatham/Noctum/workflows/CI/badge.svg)](https://github.com/SeanCheatham/Noctum/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Noctum is a local-first, AI-powered code analyzer. It runs in the background on a configured schedule to help improve your codebase.

You spent $2,000 on a high-performance laptop because you need it to be snappy and responsive while you're working. Unless you use it 24/7, that's a lot of value you're not getting out of it. Noctum helps you squeeze out a few more bits from your computer.

There's a variety of tools which offer realtime coding assistance, ranging from Cursor to Claude CLI and everywhere in-between. They're great, but they're all reliant on cloud-based models and services. Local LLM inference simply isn't fast enough for realtime coding assistance on consumer devices, which is why we're stuck with the cloud options for now. A developer doesn't want to wait 10 minutes for an answer to a simple question.

Noctum is different. Noctum doesn't work in realtime. It works asynchronously while you're off-the-clock. Your laptop can still run local inference, but not quickly enough for us impatient humans. It's still capable of doing work, just at a slower pace than the infinite server farm powering Gemini.

## Alpha Status

This project is still in development. It's not "production-ready" yet. While the project is still in alpha, releases will be backwards-incompatible.

## What does it actually do?

Install the CLI app, and specify at least one local code repository and one Ollama endpoint in the web dashboard. With the default configuration, Noctum will run from 10pm to 6am, and analyze the codebase during that time.

During this window, Noctum will step through each repository and:
- Copy the repository to a temporary directory
- Identify the types of projects in the repository
- Identify the source files for each project
- Code understanding:
   - Analyze each source file by running through LLM inference with a prompt to understand the code
- Archictural analysis:
   - Analyze each source file again by running through LLM inference with a prompt, this time focusing on extraction of architecture-related information
   - Aggregate the architecture-related information into an architectural summary
- Diagram generation:
   - Analyze each source file again by running through LLM inference with a prompt, this time focusing on extraction of information to capture into diagrams
   - Generate diagrams of the system
- Mutation testing:
   - Analyze each source file again by running through LLM inference with a prompt, this time focusing on key items for mutation testing and providing suggested mutations
   - Run each mutation through the test suite and record the results

The results are stored in a SQLite database and can be viewed in the web dashboard.

## Prerequisites

Before running Noctum, you'll need:

1. **Ollama**
   - Install from [ollama.com](https://ollama.com/)
   - Pull a code analysis model: `ollama pull qwen2.5-coder` (or your preferred model, but this has been the model used in testing)
   - Ollama must be running before starting Noctum

### Rust Projects
1. **Rust Toolchain** (1.70+)
   - Install via [rustup](https://rustup.rs/)
   - Used for analyzing Rust codebases and running mutation tests

### JS/TS Projects
1. **Node.js** (18+)
   - Install via [Node.js](https://nodejs.org/)
   - Used for analyzing JavaScript/TypeScript codebases and running mutation tests

### Other Languages
Coming "soon"

## Installation

### Quick Install Script

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
```

To uninstall (removes binary, preserves config/data):

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh -s -- --uninstall
```

NOTE: Due to complexity with environment management, the install script does not install a "service" to run Noctum in the background. You'll need to manually start Noctum using the CLI app.

### Pre-built Binaries

Download the latest release for your platform from [GitHub Releases](https://github.com/SeanCheatham/Noctum/releases):

- **Linux (x86_64):** `noctum-x86_64-unknown-linux-gnu.tar.gz`
- **macOS (Intel):** `noctum-x86_64-apple-darwin.tar.gz`
- **macOS (Apple Silicon):** `noctum-aarch64-apple-darwin.tar.gz`

Extract and place the binary in your PATH:

```bash
tar -xzf noctum-*.tar.gz
sudo mv noctum /usr/local/bin/
```

### From Source

```bash
git clone https://github.com/SeanCheatham/Noctum.git
cd Noctum
cargo install --path .
```

## Quickstart

1. **Start Ollama** (if not already running):
   ```bash
   ollama serve
   ```

2. **Create a configuration file** (optional):
   ```bash
   mkdir -p ~/.config/noctum
   touch ~/.config/noctum/config.toml
   ```
 (See [Configuration](#configuration) section)

3. **Start Noctum**:
   ```bash
   noctum start
   ```

4. **Open the dashboard** in your browser:
   ```
   http://localhost:8420
   ```

5. **Add a repository** to analyze via the dashboard UI. Be sure the repository contains a `noctum.toml` file.

Noctum will run in the background, analyzing your code according to a configured schedule.

## Configuration

Noctum looks for a config file at `~/.config/noctum/config.toml`. See [`config.example.toml`](config.example.toml) for all available options:

| Option | Default | Description |
|--------|---------|-------------|
| `web.port` | `8420` | Web dashboard port |
| `web.host` | `127.0.0.1` | Host to bind |
| `schedule.start_hour` | `22` | Start hour (0-23) of the analysis window |
| `schedule.end_hour` | `6` | End hour (0-23) of the analysis window |
| `schedule.check_interval_seconds` | `60` | How often to check schedule (seconds) |

## Repository Configuration (`noctum.toml`)

Each repository you want Noctum to analyze must contain a `noctum.toml` file in its root directory. This file controls which analysis features are enabled and how mutation testing is configured. This repository contains its own [`noctum.toml`](noctum.toml) file for reference.

### Basic Example

```toml
# Enable the features you want
# When enabled, the background worker will perform code quality analysis
enable_code_analysis = true
# When enabled, the background worker will build an architectural summary of the project
enable_architecture_analysis = true
# When enabled, the background worker will generate diagrams for the project
enable_diagram_creation = true
# When enabled, the background worker will perform mutation tests (NOTE: Requires [[mutation.rules]])
enable_mutation_testing = true

# Exclude directories from being copied to the temp directory
# This speeds up analysis and avoids issues with symlinks (e.g., node_modules/.bin)
copy_ignore = ["node_modules", "target", ".git", "dist"]

# Run this command once before baseline verification, usually to install dependencies
# CAUTION: This command is run as-is
setup_command = "cargo build"

# Mutation testing rules (required if enable_mutation_testing = true)
[[mutation.rules]]
# Files which match this pattern will invoke this rule   
glob = "src/**/*.rs"
# Before each mutation test, run this command to ensure the mutated code compiles. Mutations that don't compile are skipped.
# These commands are run from the repository root
# CAUTION: This command is run as-is
build_command = "cargo check"
# Run the test and output its result
# CAUTION: This command is run as-is
test_command = "cargo test"
# Timeout in seconds for test execution (defaults to 300)
timeout_seconds = 300
```

### Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_code_analysis` | bool | `false` | Enable per-file code analysis |
| `enable_architecture_analysis` | bool | `false` | Enable architectural summary generation |
| `enable_diagram_creation` | bool | `false` | Enable system diagram generation |
| `enable_mutation_testing` | bool | `false` | Enable mutation testing |
| `copy_ignore` | array | `[]` | Glob patterns for files/directories to exclude when copying to temp directory |
| `setup_command` | string | `null` | Command to run once before baseline verification (e.g., `"npm ci"`) |

### Mutation Rules

Each `[[mutation.rules]]` section defines how to test files matching a glob pattern:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `glob` | string | Yes | Glob pattern to match files (e.g., `"**/*.rs"`, `"src/**/*.ts"`) |
| `build_command` | string | Yes | Command to verify the code compiles |
| `test_command` | string | Yes | Command to run tests |
| `timeout_seconds` | integer | No | Test timeout in seconds (default: 300) |

### TypeScript/Node.js Projects

For TypeScript projects, use `copy_ignore` to exclude `node_modules` and use `setup_command` to reinstall dependencies:

```toml
enable_mutation_testing = true
copy_ignore = ["node_modules", "dist", ".git"]
setup_command = "npm ci"

[[mutation.rules]]
glob = "src/**/*.ts"
build_command = "npm run build"
test_command = "npm test"
timeout_seconds = 600
```

This approach:
1. Avoids copying large `node_modules` directories (faster)
2. Prevents broken symlinks in `node_modules/.bin`
3. Ensures dependencies are properly installed in the temp directory

## Architecture

Noctum is a daemon-based application written in Rust. It features a web UI/dashboard for configuration, management, and results analysis. It depends on Ollama to run inference and the Rust toolchain to interact with your project.

A SQLite database stores configurations, plans, internal notes, and results. From the dashboard, you configure repository directories for analysis.

The daemon runs constantly in the background but only performs analysis during the configured schedule window (default 10pm-6am). Outside of this window, analysis is paused.

The background processing tasks evolve over time as the agent learns the codebase. It starts by working through the code file-by-file until it has a solid understanding of the system architecture. Once it has analyzed the codebase, it uses LLM-driven mutation testing, prioritizing areas of high importance. Results are captured and interpreted by the agent with the context of the codebase, surfacing reports and recommendations.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
