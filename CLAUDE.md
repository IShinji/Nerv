# Nerv — Claude Code Instructions

Read and follow `CONVENTIONS.md` for all code style, naming, and project structure rules.

Read `NERV-PRD.md` for the full product requirements (if present in project root; it may be in a private location).

## Project Overview

Nerv is a personal AI operating system. Rust core (Gateway, hardware detection) + Rust router (`core/brain`, migrated 2026-11-16) + Python brain (Orchestrator, Agents, Memory, Tools) + Tauri GUI (Phase 2).

**Direction (decided 2026-11-14): the brain is being unified into Rust**, one module at a time. Until a module is migrated its Python implementation stays and keeps working.

## Key Architecture Decisions

- Rust and Python communicate via JSON-RPC 2.0 over stdin/stdout (newline-delimited)
- The brain handles requests concurrently and replies may arrive out of order —
  callers match on the request `id`, never on arrival order
- Rust spawns Python as a sidecar process and manages its lifecycle. That client
  lives once in `core/shared/src/brain_client.rs` (feature `brain-client`) and is
  shared by the gateway and the Tauri GUI — do not write a second one
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
- `brain.python_command` is run as a program by the gateway, so config loading rejects
  an unknown launcher (anything but `uv`, `python`, `python3` or a path to one) unless
  `NERV_ALLOW_CUSTOM_PYTHON_COMMAND=1` is set in the environment — a config-file write
  alone cannot enable it
- Config uses YAML with snake_case keys
- Any channel with a remote sender must be gated by an allow-list in config, and
  it fails closed: an empty list authorizes nobody. The brain can run shell
  commands, so an unauthenticated channel is a remote shell
- Conversation memory is scoped per sender+channel (`nerv/memory/manager.py`);
  `facts.md` is deliberately shared across a single owner's channels
- Retrieval over memory is keyword-based today. Multilingual retrieval
  (Chinese and English now, Japanese later) is a firm direction, so a hybrid of
  keyword search and **locally computed** embeddings is planned. Constraints
  for it: embeddings are always computed on this machine and conversation text
  never leaves it; the vector store is a configurable backend (local SQLite by
  default, a third-party vector database only when explicitly enabled in config,
  and then only vectors and message ids are sent, because vectors can leak the
  meaning of the text); no language-specific code on the default path, so
  adding a language means adding evaluation data, not code; it must respect the same
  sender+channel scope isolation as the keyword path; building the index must
  never add latency to a reply; and if the embedding service is unavailable it
  falls back to keyword search. The planned implementation is in Rust, so do
  not extend the Python keyword search beyond bug fixes.
- Memory tools (`nerv/tools/memory.py`) read the scope the orchestrator binds
  via `use_scope`; it fails closed to the default scope, so a tool can never
  read another sender's history. Anything reaching memory from a new entry
  point must bind the scope the same way
- Past days outside the recency window survive as rolling summaries generated
  off the reply path (`nerv/memory/summarizer.py`). Summarizing must never add
  latency to a reply or fail it

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

1. `check_pending` (Python): resolve `confirm <id>` / `cancel <id>` from the
   pending queue. Must stay first — an approval never depends on a model call.
2. The migrated module runs in-process in Rust (no round trip).
3. Thin Python RPC methods for what remains (`execute` = orchestrator
   dispatch). Responses keep the old wire shape so hosts render unchanged.

- The facade lives in `core/brain/src/handle.rs` and is the single host entry:
  the gateway and the GUI both call it — do not duplicate the split logic in a
  second host
- Prompts of migrated modules live as data files under `core/brain/prompts/`
- Follow the same pattern for every later module (memory, tools, orchestrator,
  …), splitting methods until the Python sidecar is gone

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
