# Nerv — Agent Instructions (for Codex / AI Coding Tools)

Read and follow `CONVENTIONS.md` for all code style, naming, and project structure rules.

## Project Overview

Nerv is a personal AI operating system. Architecture: Rust core + Python brain + Tauri GUI (later).

- `core/` — Rust: Gateway (channel adapters), hardware detection, process management
- `brain/` — Python: Router, Orchestrator, Agent Factory, Context Manager, Memory, Tools
- `agents/` — YAML agent definitions
- `gui/` — Tauri + React (Phase 2, not yet)

## Communication

Rust ↔ Python: JSON-RPC 2.0 over stdin/stdout, newline-delimited.

## Critical Design Rules

- Router MUST use local Ollama model (not paid API) — token cost is the #1 concern
- Gateway MUST use ChannelAdapter trait — never hardcode Telegram logic in core
- All messages MUST convert to unified Message format
- Agent definitions MUST be YAML files, not Python code
- NEVER send full conversation history to LLMs — use rolling summaries

## Code Style

See `CONVENTIONS.md` for complete rules. Summary:
- Rust: rustfmt + clippy, snake_case functions, PascalCase types, async tokio, thiserror + anyhow
- Python: ruff format + check, type hints required, dataclass/Pydantic for data, asyncio
- Git: `feat:` / `fix:` / `refactor:` prefix, English, lowercase
