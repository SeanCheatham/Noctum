# Noctum

[![CI](https://github.com/SeanCheatham/Noctum/workflows/CI/badge.svg)](https://github.com/SeanCheatham/Noctum/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Noctum is a local-first, AI-powered code analyzer. It runs in the background, taking advantage of idle compute time to help improve your codebase.

You spent $2,000 on a high-performance laptop because you need it to be snappy and responsive while you're working. Unless you use it 24/7, that's a lot of value you're not getting out of it. Noctum helps you squeeze out a few more bits from your computer.

There's a variety of tools which offer realtime coding assistance, ranging from Cursor to Claude CLI and everywhere in-between. They're great, but they're all reliant on cloud-based models and services. Local LLM inference simply isn't fast enough for realtime coding assistance on consumer devices, which is why we're stuck with the cloud options for now. A developer doesn't want to wait 10 minutes for an answer to a simple question.

Noctum is different. Noctum doesn't work in realtime. It works asynchronously while you're off-the-clock. Your laptop can still run local inference, just not quickly enough for us impatient humans. It's still capable of doing work, just at a slower pace than the infinite server farm powering Gemini.

## Prerequisites

Before running Noctum, you'll need:

1. **Rust Toolchain** (1.70+)
   - Install via [rustup](https://rustup.rs/): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - Used for analyzing Rust codebases and running mutation tests

2. **Ollama**
   - Install from [ollama.com](https://ollama.com/)
   - Pull a code analysis model: `ollama pull qwen2.5-coder` (or your preferred model)
   - Ollama must be running before starting Noctum

## Installation

### From Source

```bash
git clone https://github.com/SeanCheatham/Noctum.git
cd Noctum
cargo install --path .
```

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

### Quick Install Script

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh
```

To install and run as a background service (starts automatically on boot):

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh -s -- --service
```

This installs a systemd service on Linux or a launchd agent on macOS.

To uninstall (removes binary and services, preserves config/data):

```bash
curl -fsSL https://raw.githubusercontent.com/SeanCheatham/Noctum/main/install.sh | sh -s -- --uninstall
```

## Quickstart

1. **Start Ollama** (if not already running):
   ```bash
   ollama serve
   ```

2. **Create a configuration file** (optional):
   ```bash
   mkdir -p ~/.config/noctum
   cp config.example.toml ~/.config/noctum/config.toml
   ```

3. **Start Noctum**:
   ```bash
   noctum start
   ```

4. **Open the dashboard** in your browser:
   ```
   http://localhost:8420
   ```

5. **Add a repository** to analyze via the dashboard UI

Noctum will run in the background, analyzing your code during idle periods (by default, when the system has been idle for 5+ minutes).

## Configuration

Noctum looks for a config file at `~/.config/noctum/config.toml`. See [`config.example.toml`](config.example.toml) for all available options:

| Option | Default | Description |
|--------|---------|-------------|
| `web.port` | `8420` | Web dashboard port |
| `web.host` | `127.0.0.1` | Host to bind |
| `ollama.url` | `http://localhost:11434` | Ollama API endpoint |
| `ollama.model` | `qwen2.5-coder` | Model for analysis |
| `idle.threshold_seconds` | `300` | Seconds of idle before starting work |

## Features

### Implemented

- Rust-oriented code analysis with LLM-powered insights
- LLM-driven mutation testing
- Web dashboard for configuration and results
- Idle detection and scheduled analysis
- Multi-endpoint Ollama support
- SQLite database for persistent storage

### Roadmap

- Multi-language support (beyond Rust)
- Automated unit test development
- Code documentation generation
- Code cleanup suggestions
- Language translation (e.g., C to Rust)
- Architectural diagram creation

## Architecture

Noctum is a daemon-based application written in Rust. It features a web UI/dashboard for configuration, management, and results analysis. It depends on Ollama to run inference and the Rust toolchain to interact with your project.

A SQLite database stores configurations, plans, internal notes, and results. From the dashboard, you configure repository directories for analysis.

The daemon runs constantly in the background but monitors for user inactivity. When inactive, the daemon starts its background processing tasks. If the user comes back, the background processing is paused.

The background processing tasks evolve over time as the agent learns the codebase. It starts by working through the code file-by-file until it has a solid understanding of the system architecture. Once it has analyzed the codebase, it uses LLM-driven mutation testing, prioritizing areas of high importance. Results are captured and interpreted by the agent with the context of the codebase, surfacing reports and recommendations.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
