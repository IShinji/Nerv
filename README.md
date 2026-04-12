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
│   Channel adapters · Auth · Queue       │
├─────────────────────────────────────────┤
│         Router (Local model, ~200 tok)  │
│   Intent classify · Model select        │
├────────┬────────┬────────┬──────────────┤
│ Coder  │Research│ Writer │ Auto-created │  ← Agent Pool
│ Haiku  │Sonnet  │ Gemini │ (on demand)  │
├────────┴────────┴────────┴──────────────┤
│         Context Manager (Python)        │
│   Rolling summary · NOT full history    │
├──────────────────┬──────────────────────┤
│   Memory         │   Tools              │
│   Markdown+SQLite│   Shell·Browser·API  │
└──────────────────┴──────────────────────┘
```

**Key insight:** OpenClaw sends your entire conversation history (~120K tokens) with every single message. Nerv sends a 500-token summary. That's a 99.6% reduction.

---

## ✨ Core Features

### 🧩 Multi-Agent with Auto-Creation
No matching agent for your task? Nerv creates one on the spot, saves it, and reuses it next time. Over time you build a personal team of experts — from tax advisors to recipe planners.

### 📈 Agents That Evolve
Agents aren't disposable. They track their domain, update their knowledge when the field changes, and keep a version history you can roll back. Think of them as employees who read industry journals.

### ⚙️ Dual-Track Automation (Skills vs Workflows)
Nerv separates open-ended thinking from rigid execution:
- **Skills**: Markdown guidelines that teach Agents *how* to think and evaluate (e.g., "How to review a resume").
- **Workflows**: Hardcoded YAML steps executed by a native `WorkflowExecutor`. When an agent needs to perform a 10-step UI automation, it delegates it to the Executor, bypassing LLM hallucination entirely for absolute predictability.
- **Parsimony Rule**: Agents are instructed to strictly prefer fast, lightweight API tools over heavy UI automation workflows automatically.

### 💰 Brutal Token Efficiency
A local model (free, runs on your machine) handles routing and heartbeats. Only complex tasks hit paid APIs. Model tiering:

| Tier | Model | Cost | Use case |
|------|-------|------|----------|
| 0 | Local Ollama (Qwen 7B) | $0 | Routing, simple Q&A, heartbeat |
| 1 | Gemini Flash / Groq Llama | Free tier | Code, summarization |
| 2 | Haiku / Gemini Pro | Low | Complex code, research |
| 3 | Sonnet / Opus | Market | Architecture, critical reasoning |

### 🔌 Channel-Agnostic
Telegram is just one adapter. The Gateway speaks a universal `Message` format. Adding a new channel = one new file, zero changes to the brain.

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
- macOS (Apple Silicon) or Linux (NVIDIA GPU)
- Rust (latest stable)
- Python 3.11+
- [Ollama](https://ollama.ai) installed

### Install & Run
```bash
git clone https://github.com/IShinji/Nerv.git
cd Nerv

# Setup python environment
cd brain
uv sync

# Run the gateway
cd ../core/gateway
cargo run
```

---

## Design Principles

1. **Token is money** — Every LLM call needs justification. If a lightweight local 3B model can do it, local does it.
2. **Local first** — Your data (memory, configuration) never leaves your machine.
3. **If it doesn't exist, create it** — Unknown task type? The Agent Factory will generate a specialist agent automatically.
4. **Fully decoupled** — Channels, models, tools, agents — all pluggable, nothing hardcoded.

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
