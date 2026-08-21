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
- The model backend is a config switch via a provider-prefix scheme on the model string
  (`claude-cli:<alias>` | `local:<ollama-model>` | `litellm:<model>`), resolved by the
  unified LLM client in `brain/nerv/llm/`. Currently every tier runs on `claude-cli:opus`
  (Claude Code subscription) for validation; local Ollama is reserved and dormant — switch
  back by editing `models.tiers` in config. Never hardcode model names; read them from config.
- Under the `claude-cli` backend, Claude Code runs its own tool loop; Nerv native tools are
  exposed to it through the MCP server in `brain/nerv/mcp_server/`. Tool calls that touch
  paths outside `security.workspace_root` (or `shell`/`desktop_control`) are suspended into
  the shared pending-action queue and require user `confirm <id>`.
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

## GUI

- The Tauri + React desktop app lives in `gui/`. Its Rust backend (`gui/src-tauri`)
  spawns the Python brain as a sidecar and exposes Tauri commands (`send_message`,
  `list_agents`) to the React frontend. Keep GUI logic thin — the brain owns behavior.

## What NOT To Do

- Do not use `.env` files for config — use `~/.nerv/config.yaml`
- Do not hardcode model names — read from config
- Do not send full conversation history to LLMs — use rolling summaries
