# Noctum

Noctum is a local-first, AI-powered code analyzer. It runs in the background, taking advantage of idle compute time to help imporove your codebase.

You spent $2,000 on a high-performance laptop because you need it to be snappy and responsive while you're working. Unless you use it 24/7, that's a lot of value you're not getting out of it. Noctum helps you squeeze out a few more bits from your computer.

There's a variety tools which offer realtime coding assistance, ranging from Cursor to Claude CLI and everywhere in-between. They're great, but they're all reliant on cloud-based models and services. Local LLM inference simply isn't fast enough for realtime coding assistance on consumer devices, which is why we're stuck with the cloud options for now. A developer doesn't want to wait 10 minutes for an answer to a simple question.

Noctum is different. Noctum doesn't work in realtime. It works asynchronously while you're off-the-clock. Your laptop can still run local inference, just not quickly enough for us impatient humans. It's still capable of doing work, just at a slower pace than the infinite server farm powering Gemini.

## Features

This project is still under development. The features listed here are a mixture of what _is_ and what _is coming_.

### MVP

- Rust-oriented code analysis
- Automated mutation testing

### Future Directions

- Multi-language support (beyond Rust)
- Automated unit test development
- Code documentation
- Code cleanup
- Language translation (i.e. C to Rust)
- Architectural diagram creation
- Docs maintenance

## Architecture

Noctum is a daemon-based application written in Rust. It features a web UI/dashboard for configuration, management, and results analysis. It depends on Ollama (prerequisite) to run inference and the Rust toolchain to interact with your project.

A SQLite database is used to store configurations, plans, internal notes, and results. From the dashboard, a user configures repo directories for analysis.

The daemon runs constantly in the background, but it monitors for user inactivity. When inactive, the daemon starts its background processing tasks. If the user comes back, the background processing is paused.

The background processing tasks evolve over time as the agent learns the codebase. It starts off by working through the code file-by-file until it has a solid understanding of the system architecture. Once it has analyzed the codebase, it leverages cargo-mutants to run mutation testing, prioritizing areas of high-importance. Results are captured and interpreted by the agent with the context of the codebase. The agent surfaces these reports and provides recommendations.
