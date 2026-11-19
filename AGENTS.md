# Nerv — Agent Instructions (for Codex / AI Coding Tools)

Read and follow `CONVENTIONS.md` for all code style, naming, and project structure rules.

## Project Overview

Nerv is a personal AI operating system. Architecture: 100% Rust brain (`core/`: gateway, router, orchestrator, memory, tools, workflows, MCP, proactive heartbeat) + Tauri GUI.

- `core/` — Rust: gateway (channel adapters), hardware detection, the whole brain (`core/brain/`), process management
- `agents/` — YAML agent definitions
- `gui/` — Tauri + React

## Communication

Everything runs in-process: hosts call `nerv_brain::handle::BrainHandle` directly. The old JSON-RPC sidecar protocol is retired.

## Critical Design Rules

- Router MUST use local Ollama model (not paid API) — token cost is the #1 concern
- Gateway MUST use ChannelAdapter trait — never hardcode Telegram logic in core
- All messages MUST convert to unified Message format
- Agent definitions MUST be YAML files, not Python code
- NEVER send full conversation history to LLMs — use rolling summaries

## Code Style

See `CONVENTIONS.md` for complete rules. Summary:
- Rust: rustfmt + clippy, snake_case functions, PascalCase types, async tokio, thiserror + anyhow
- Git: `feat:` / `fix:` / `refactor:` prefix, English, lowercase
