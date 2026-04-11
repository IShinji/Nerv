# Nerv — Claude Code Instructions

Read and follow `CONVENTIONS.md` for all code style, naming, and project structure rules.

Read `NERV-PRD.md` for the full product requirements (if present in project root; it may be in a private location).

## Project Overview

Nerv is a personal AI operating system. Rust core (Gateway, hardware detection) + Python brain (Router, Orchestrator, Agents) + Tauri GUI (Phase 2).

## Key Architecture Decisions

- Rust and Python communicate via JSON-RPC 2.0 over stdin/stdout (newline-delimited)
- Rust spawns Python as a sidecar process and manages its lifecycle
- Gateway uses a `ChannelAdapter` trait — Telegram is just one implementation
- All messages are converted to a unified `Message` struct before reaching the brain
- Router runs on a LOCAL model (Ollama) to minimize token cost — this is critical
- Agent definitions are YAML files, not code
- Config uses YAML with snake_case keys

## When Writing Code

- Run `cargo fmt` and `cargo clippy` before committing Rust code
- Run `ruff format` and `ruff check` before committing Python code
- Add tests for new functionality
- Use async for all I/O operations
- Never use `.unwrap()` in non-test Rust code
- All public Python functions must have type hints
- Keep dependencies minimal — justify any new addition

## What NOT To Do

- Do not add GUI/Tauri code yet (Phase 2)
- Do not use `.env` files for config — use `~/.nerv/config.yaml`
- Do not hardcode model names — read from config
- Do not send full conversation history to LLMs — use rolling summaries
