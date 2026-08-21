# Nerv GUI (Tauri + React)

Desktop app for Nerv. The React frontend talks to a thin Rust (Tauri) backend
that spawns the Python brain as a sidecar and speaks JSON-RPC to it.

## Architecture

```
React UI (src/)
  │  @tauri-apps/api invoke()
  ▼
Tauri Rust backend (src-tauri/)  ── spawns ──▶  Python brain (uv run python -m nerv)
  commands: send_message, check_heartbeat, list_agents      JSON-RPC over stdin/stdout
```

The backend keeps GUI logic thin — all behavior (routing, tools, memory,
proactivity) lives in the brain.

## Develop

```bash
cd gui
npm install
npm run tauri dev      # launches the window + spawns the brain
```

## Build

```bash
npm run build          # frontend (tsc + vite)
npm run tauri build    # full desktop bundle
```

## Notes

- Requires `uv` on PATH (the backend launches the brain via `uv run python -m nerv`).
- The window polls the brain heartbeat every 30s to surface proactive reminders.
- `src-tauri/icons/icon.png` is a placeholder — replace with real app icons
  before shipping a bundle.
