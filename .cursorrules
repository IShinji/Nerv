Read and follow CONVENTIONS.md for all code rules.

This is Nerv, a personal AI operating system. Rust core (core/) + Python brain (brain/) + Tauri GUI (gui/, Phase 2).

Key rules:
- Rust ↔ Python: JSON-RPC 2.0 over stdin/stdout
- Router uses LOCAL Ollama model, not paid API
- Gateway uses ChannelAdapter trait, Telegram is one implementation
- Agent definitions are YAML, not code
- Never send full conversation history — use rolling summaries
- Rust: rustfmt, clippy, async tokio, thiserror + anyhow, no .unwrap()
- Python: ruff, type hints required, asyncio, dataclass/Pydantic
- See CONVENTIONS.md for full details
