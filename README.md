# 🧠 Nerv

**Your personal AI nerve center — one interface, a team of evolving experts.**

Nerv is an open-source personal AI operating system that gives you a single point of contact (Telegram, Email, Slack, or any channel you want) to command a team of specialized AI agents. Unlike chatbots that forget and frameworks that burn your wallet, Nerv routes every task to the right model at the right cost — and its agents get smarter over time.

---

## Why Nerv?

**The problem:** Current AI assistants are either expensive black boxes (ChatGPT, Claude Pro) or token furnaces that cost $18 overnight doing nothing (OpenClaw's heartbeat sending 120K tokens to check if it's daytime). You shouldn't need a PhD to set one up, and you shouldn't need a trust fund to run one.

**Nerv's answer:**

| Pain point | How Nerv solves it |
|---|---|
| 🔥 Token waste | A local router classifies your message in ~200 tokens, then picks the cheapest model that can handle it |
| 🔒 Single model | Multi-agent: Coder, Researcher, Writer — or auto-create a new expert on the fly |
| 💀 Agents forget | Agents are permanent team members. They evolve with their field — your tax advisor updates itself when tax law changes |
| 📱 Locked to one app | Channel-agnostic: Telegram today, Slack tomorrow, email next week. One adapter, zero core changes |
| 🤯 CLI-only setup | GUI desktop app (Tauri). Hardware auto-detection. One-click model install. Your grandma could set it up* |

<sub>*your grandma's results may vary</sub>

---

## Architecture

```
You (Telegram / Slack / Email / CLI / WebChat)
  │
  ▼
┌─────────────────────────────────────────┐
│         Gateway (Rust)                  │
│  Adapters · Sender allow-list · Queue   │
│  Supervises the brain (restart, timeout)│
├─────────────────────────────────────────┤
│         Router (Local model, ~200 tok)  │
│   Intent classify · Model select        │
├────────┬────────┬────────┬──────────────┤
│ Coder  │Research│ Writer │ Auto-created │  ← Agent Pool
│ Haiku  │Sonnet  │ Gemini │ (on demand)  │
├────────┴────────┴────────┴──────────────┤
│         Context Manager (Python)        │
│   Budgeted window · NOT full history    │
├──────────────────┬──────────────────────┤
│   Memory         │ Capability Layer     │
│   Markdown files │ Native Tools · MCP   │
└──────────────────┴──────────────────────┘
```

**Key insight:** OpenClaw sends your entire conversation history (~120K tokens) with every single message. Nerv sends a bounded window instead: the agent's system prompt, a long-term facts file, and only as many recent messages as fit the agent's token budget — oldest first to be dropped. Summarizing that window rather than truncating it is a planned improvement, not what ships today.

**Integration direction:** Nerv should be `MCP-first` for external apps and services. If a stable MCP server already exists for Chrome, GitHub, Notion, or another system, Nerv should use that before adding a bespoke integration. Native tools remain critical for core local capabilities and as fallbacks.

---

## Implementation Status

Nerv is a working system, not a design doc. What exists today, and where to read it:

| Subsystem | Status | Where |
|---|---|---|
| **Router** — intent classification + model tier selection from a small dedicated prompt, with an offline eval harness | ✅ Built | `brain/nerv/router/`, `brain/nerv/eval.py` |
| **Orchestrator + Agent Factory** — task dispatch, runtime policy injection, on-demand specialist agent creation | ✅ Built | `brain/nerv/orchestrator/` |
| **Tool / function calling** — decorator-based registry that derives JSON schemas from Python signatures; **31 registered tools** | ✅ Built | `brain/nerv/tools/` |
| **MCP client** — connect to external MCP servers, with builtin presets (e.g. Chrome DevTools) and a diagnostics tool | ✅ Built | `brain/nerv/mcp/` |
| **MCP server** — exposes Nerv's own tools over MCP, so a host like the Claude Code CLI can drive Nerv | ✅ Built | `brain/nerv/mcp_server/` |
| **Guardrails** — workspace sandbox policy; outward-acting tools (`shell`, `desktop_control`, `send_email`) and any path escaping the sandbox are suspended into a cross-process, file-locked approval queue and only run on explicit `confirm <id>` | ✅ Built | `brain/nerv/security/` |
| **Channel authorization** — every channel with a remote sender requires an explicit allow-list. Telegram fails closed: an empty `allowed_users` authorizes nobody and the gateway refuses to start the adapter | ✅ Built | `core/shared/src/config.rs`, `core/gateway/src/adapter/` |
| **Brain supervision** — the JSON-RPC client fails in-flight requests when the brain dies, respawns it up to `restart_max`, and bounds every call with a timeout | ✅ Built | `core/shared/src/brain_client.rs` |
| **Agent memory** — conversation history partitioned by day *and* by sender/channel scope (JSON Lines, append-only, file-locked across processes), a long-term `facts.md` shared by the owner across channels, and a context manager that assembles a token-budgeted window (system prompt + facts + newest-first history, truncated to fit) rather than replaying full history | ✅ Built | `brain/nerv/memory/` |
| **Context summarization** — summarizing the dropped history instead of truncating it | 📋 Roadmap | — |
| **Pluggable model backends** — provider-prefix scheme (`claude-cli:` / `local:` / `litellm:`) so swapping providers is a config edit, not a code change | ✅ Built | `brain/nerv/llm/` |
| **Skills / Workflows** — markdown skill guidelines and YAML workflows run by a native executor | ✅ Built | `brain/nerv/skills/`, `brain/nerv/workflows/` |
| **Gateway** — Rust daemon, channel adapters, IPC to the brain | ✅ Built | `core/` |
| **GUI** — Tauri desktop shell | 🚧 Early | `gui/` |
| **Agent evolution** — self-updating agent knowledge with version history and rollback | 🚧 Partial | `brain/nerv/evolution.py` |
| **Agent Store** | 📋 Roadmap | — |
| **Vector retrieval / RAG** | ❌ Not built — memory is file-backed and summary-based by design | — |

**Size:** 7.9k lines of Python in `brain/nerv/`, 2.1k lines of Rust in `core/`, and 2.7k lines of Python tests. **All green in CI** (`.github/workflows/ci.yml`):

| Suite | Command | Tests |
|---|---|---|
| Brain | `cd brain && uv run pytest` | 128 |
| Core (Rust) | `cd core && cargo test --workspace` | 36 |
| GUI (Rust) | `cd gui/src-tauri && cargo test` | 4 |

CI also enforces `ruff check`, `ruff format --check`, `cargo fmt --check`, and
`cargo clippy -D warnings`.

---

## ✨ Core Features

### 🧩 Multi-Agent with Auto-Creation
No matching agent for your task? Nerv creates one on the spot, saves it, and reuses it next time. Over time you build a personal team of experts — from tax advisors to recipe planners.

### 📈 Agents That Evolve
Agents aren't disposable. They track their domain, update their knowledge when the field changes, and keep a version history you can roll back. Think of them as employees who read industry journals.

### ⚙️ Capability Layer + Dual-Track Automation
Nerv separates capability access from execution strategy:
- **Capability Layer**: Core local actions stay as native tools (`shell`, files, memory, notifications). External apps and SaaS integrations should prefer MCP servers when available.
- **MCP-first Browsering**: For interactive browser work, Nerv should prefer Chrome MCP (or another browser MCP server) over bespoke UI scripting. The current native `chrome_browser` path is an MVP fallback, not the long-term integration model.
- **Skills**: Markdown guidelines that teach Agents *how* to think and evaluate (e.g., "How to review a resume").
- **Workflows**: Hardcoded YAML steps executed by a native `WorkflowExecutor`. When an agent needs to perform a 10-step operational sequence, it delegates it to the Executor for predictability.
- **Parsimony Rule**: Agents are instructed to prefer direct APIs and MCP-backed tools over heavy GUI automation. UI workflows are the fallback when lighter providers cannot complete the task.

### 💰 Brutal Token Efficiency
A local model (free, runs on your machine) handles routing and heartbeats. Only complex tasks hit paid APIs. Model tiering:

| Tier | Model | Cost | Use case |
|------|-------|------|----------|
| 0 | Local Ollama (Qwen 7B) | $0 | Routing, simple Q&A, heartbeat |
| 1 | Gemini Flash / Groq Llama | Free tier | Code, summarization |
| 2 | Haiku / Gemini Pro | Low | Complex code, research |
| 3 | Sonnet / Opus | Market | Architecture, critical reasoning |

The backend is a config switch via a provider-prefix scheme on the model string
(`claude-cli:<alias>` | `local:<ollama-model>` | `litellm:<model>`). **During the
current validation phase every tier runs on `claude-cli:opus`** (your Claude Code
subscription — no API key, no per-token bill). The tiered local/cloud routing above
is the reserved target design: swap it back any time by editing `models.tiers` in
config — no code changes.

### 🔌 Channel-Agnostic
Telegram is just one adapter. The Gateway speaks a universal `Message` format. Adding a new channel = one new file, zero changes to the brain.

Each remote channel is gated by an allow-list, because the brain can run shell
commands and read your filesystem. Conversation history is stored per sender,
so two people on the same instance never see each other's messages.

### 🧬 Portable & Replicable
```
~/.nerv/
├── core/        ← git clone this for a new user
├── personal/    ← your data, never leaves your machine
└── evolving/    ← auto-updating knowledge
```

### 🏪 Agent Store (Roadmap)
Share your best agents with the community. Install a battle-tested "Legal Advisor" agent with one click. Like an app store, but for AI expertise.

---

## Quick Start

> ⚠️ **Nerv is currently in early active development.** The core architecture is functional and we are building out additional integrations. Star the repo to follow along.

### Prerequisites
- macOS (Apple Silicon) or Linux
- Rust (latest stable)
- Python 3.11+ and [uv](https://docs.astral.sh/uv/)
- A model backend — the default config uses the local [Claude Code](https://claude.com/claude-code) CLI (no API key). [Ollama](https://ollama.ai) is only needed if you switch a tier to a `local:` model.

### Install & Run
```bash
git clone https://github.com/IShinji/Nerv.git
cd Nerv

# Setup python environment
cd brain
uv sync

# Run the test suite (115 tests, ~1s)
uv run pytest

# Run the gateway
cd ../core/gateway
cargo run
```

### Enabling Telegram

Nerv's tools can run shell commands and read your filesystem, so the Telegram
adapter refuses to start without an explicit allow-list. In `config.yaml`:

```yaml
channels:
  telegram:
    enabled: true
    bot_token: "<from @BotFather>"
    allowed_users: ["123456789"]   # your id — ask @userinfobot
```

An empty `allowed_users` authorizes nobody; the gateway logs an error and skips
the adapter rather than exposing an open bot.

---

## Design Principles

1. **Token is money** — Every LLM call needs justification. If a lightweight local 3B model can do it, local does it.
2. **Local first** — Your data (memory, configuration) never leaves your machine.
3. **If it doesn't exist, create it** — Unknown task type? The Agent Factory will generate a specialist agent automatically.
4. **MCP-first for integrations** — Prefer standard MCP servers for external tools and applications; keep native integrations for core local capabilities and fallback paths.
5. **Fully decoupled** — Channels, models, capability providers, agents — all pluggable, nothing hardcoded.

---

## Tech Stack

| Layer | Technology | Why |
|-------|-----------|-----|
| Core / Gateway | **Rust** | Zero-GC background daemon, native perf, IPC handling |
| Brain / Agents | **Python** | AI ecosystem, ML integrations, dynamic typing |
| Local Models | **Ollama** | Dead simple local inference |

---

## Contributing

We welcome contributions in Rust, Python, and documentation.

1. ⭐ **Star this repo** 
2. 🐛 **Open issues** — feature requests, architecture feedback, design ideas
3. 🔧 **PRs** — feel free to submit pull requests

---

## License

MIT — use it, fork it, build on it.

---

<p align="center">
  <b>Nerv</b> — Your specialized AI operating system.
</p>
