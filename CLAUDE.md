# Nerv — Claude Code Instructions

Read and follow `CONVENTIONS.md` for all code style, naming, and project structure rules.

Read `NERV-PRD.md` for the full product requirements (if present in project root; it may be in a private location).

## Project Overview

Nerv is a personal AI operating system. **100% Rust brain** (gateway + router + orchestrator + memory + tools + workflows + MCP + proactive heartbeat, in `core/`) + Tauri GUI (Phase 3). The python brain is fully retired (migration completed 2026-11-19).

## Key Architecture Decisions

- The whole brain is one Rust process: hosts (gateway, GUI) call
  `nerv_brain::handle::BrainHandle` directly — no IPC, no sidecar
- Gateway uses a `ChannelAdapter` trait — Telegram is just one implementation
- All messages are converted to a unified `Message` struct before reaching the brain
- The model backend is a config switch via a provider-prefix scheme on the model string
  (`claude-cli:<alias>` | `local:<ollama-model>` | `litellm:<model>`), resolved by the
  unified LLM client in `core/brain/src/llm/`. claude-cli runs the Claude Code
  subscription CLI; litellm serves the anthropic / OpenAI-compatible (openai, deepseek) /
  gemini cloud families natively. Currently every tier runs on `claude-cli:opus`
  for validation; local Ollama is reserved and dormant — switch
  by editing `models.tiers` in config. Never hardcode model names; read them from config.
- Under the `claude-cli` backend, Claude Code runs its own tool loop; Nerv native tools are
  exposed to it through the native MCP server (`--mcp-server` mode of the gateway/GUI
  binary). Tool calls that touch paths outside `security.workspace_root` (or
  `shell`/`desktop_control`) are suspended into the shared pending-action queue and
  require user `confirm <id>`.
- Agent definitions are YAML files, not code
- Config uses YAML with snake_case keys
- Any channel with a remote sender must be gated by an allow-list in config, and
  it fails closed: an empty list authorizes nobody. The brain can run shell
  commands, so an unauthenticated channel is a remote shell
- Conversation memory is scoped per sender+channel (`core/brain/src/memory/`);
  `facts.md` is deliberately shared across a single owner's channels. A tool
  can never read another sender's history — anything reaching memory from a
  new entry point must bind the scope the same way (fail closed to default)
- Retrieval over memory is a hybrid of keyword search and **locally computed**
  embeddings (opt-in via `brain.vector_retrieval`). Constraints: embeddings
  are always computed on this machine and conversation text never leaves it;
  the vector store is a configurable backend (local SQLite by default, a
  third-party vector database only when explicitly enabled in config, and then
  only vectors and message ids are sent, because vectors can leak the meaning
  of the text); no language-specific code on the default path, so adding a
  language means adding evaluation data, not code; it must respect the same
  sender+channel scope isolation as the keyword path; building the index must
  never add latency to a reply; and if the embedding service is unavailable it
  falls back to keyword search
- Past days outside the recency window survive as rolling summaries generated
  off the reply path (`core/brain/src/memory/summarizer.rs`). Summarizing must
  never add latency to a reply or fail it

## Rust migration rules

- **Migrate module by module, never all at once.** The JSON-RPC boundary stays
  until the last Python module is gone. Port a module's Python tests to Rust
  first; delete the Python implementation only after they pass.
- **New features are written in Rust.** Long-running or proactive features
  (background jobs, addressed notifications, schedulers) live in the gateway,
  not in the brain, so a brain restart cannot lose them.
- **The Python brain only receives bug fixes and small, self-contained
  hardening** (for example scrubbing credential-like environment variables).
  Do not add features to it unless the feature is blocked on a module that has
  not been migrated yet; say so in the PR.
- **Prompts and agent definitions stay data files** (YAML / markdown), not Rust
  source, so they can be iterated on without a compile.
- Behaviour must not change during a migration. Where practical, compare old
  and new output on the same inputs.

### Transition mechanics (the per-module pattern)

The JSON-RPC method surface is not frozen: when a module migrates, the old
method that used it is split, and the host-side facade drives the new flow.
The router is the worked example — the old `route` method became:

1. Pending commands run in Rust (`turn::prepare::handle_pending_command`):
   resolve `confirm <id>` / `cancel <id>` from the pending queue first — an
   approval never depends on a model call. Python-only tools are confirmed
   through the `execute_pending` RPC.
2. The migrated module runs in-process in Rust (no round trip).
3. Thin Python RPC methods remain only for what is still python:
   `run_tool` (unported tools), `workflow_fallback` (the WorkflowExecutor),
   `complete_turn` (litellm backends), `queue_notifications` (heartbeat
   alerts), `heartbeat`, `ping`. Responses keep the old wire shape so hosts
   render unchanged.

- The facade lives in `core/brain/src/handle.rs` and is the single host entry:
  the gateway and the GUI both call it — do not duplicate the split logic in a
  second host
- Prompts of migrated modules live as data files under `core/brain/prompts/`
- Follow the same pattern for every later module (tools, orchestrator
  leftovers, …), splitting methods until the Python sidecar is gone

## When Writing Code

- Run `cargo fmt` and `cargo clippy` before committing Rust code
- Run `ruff format` and `ruff check` before committing Python code (both are in
  the dev dependency group; CI enforces them)
- Builtin tools live in focused modules (`nerv/tools/{web,files,system,knowledge,desktop,memory}.py`).
  `nerv/tools/builtins.py` only re-exports them, so patching a name *there* does
  not affect the module that defines it — tests must patch the owning module
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

- Do not use `.env` files for config — use `config.yaml` (user overrides on top of
  `core/config.default.yaml`). The gateway and the brain both look for
  `$NERV_CONFIG` first, then `<project root>/config.yaml`, then
  `~/.nerv/config.yaml` (`brain/nerv/config.py` mirrors the Rust loader; keep the
  two in step)
- Do not hardcode model names — read from config
- Do not send full conversation history to LLMs — use rolling summaries
