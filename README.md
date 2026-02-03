# Noctum

Noctum is an AI-powered sandboxed simulation testing tool. It helps developers validate their applications through safe, isolated integration testing using LLM-driven analysis.

## The Problem

LLMs can generate full-stack applications, but validating that code in realistic environments is hard. Unit tests catch isolated issues, but integration problems—race conditions, state management bugs, incorrect API contracts—slip through. And letting an LLM freely run integration tests against real systems is dangerous.

## The Solution

Noctum provides a sandboxed environment where an LLM can safely:

1. **Run your actual application** (not just unit tests)
2. **Observe real behavior** via logs
3. **Analyze failures** and understand what went wrong
4. **Propose and test fixes** iteratively
5. **All without risk** of escaping the sandbox

## How It Works

1. **You provide a test harness** — a script or entry point that runs your application with mocked external dependencies (databases, APIs, etc.)
2. **Noctum runs it in Docker** — isolated container with no network access
3. **Logs are collected and analyzed** — the LLM reviews output to understand behavior
4. **Success or failure is determined** — MVP: runs for a configured duration without crashing
5. **Iterate** — if it fails, the LLM can analyze, suggest fixes, and re-test

## Quick Start

```bash
# Install dependencies
npm install

# Build the project
npm run build

# Start the web dashboard
npx noctum serve --dir /path/to/your/project
```

Options:

- `--port <number>` — Port to listen on (default: random available port)
- `--host <address>` — Host to bind to (default: 127.0.0.1)
- `--dir <path>` — Target project directory (default: current directory)

Open the displayed URL in your browser to access the dashboard.

## Status

🚧 **Early Development** — Core simulation workflow is functional. Iterative fix-and-retry loop is not yet implemented.

### What Works

- [x] Web dashboard with server health monitoring
- [x] Multi-step onboarding wizard (git status → directory setup → compose generation)
- [x] Claude Agent SDK integration for codebase analysis
- [x] Docker Compose generation — LLM analyzes your code and generates isolated container configuration
- [x] Docker sandbox execution — builds and runs containers with network isolation
- [x] Log collection and LLM analysis — collects container logs, determines success/failure

### Planned Features

- [ ] Iterative failure analysis and fix suggestions
- [ ] Test harness scaffolding (LLM helps generate your test harness)
- [ ] Configuration via `noctum.toml`

## Future Ideas

- Mutation testing (mutate code, verify tests catch it)
- Harness mutation (mutate mock behavior, verify app handles it correctly)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
