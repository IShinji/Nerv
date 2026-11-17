#!/usr/bin/env bash
set -euo pipefail

# Nerv — Environment Bootstrap Script
# Checks required tools (Rust, uv, Python), builds, and creates config.yaml.
# Never pipes a downloaded script into a shell; prints official instructions instead.
# Idempotent: safe to run multiple times.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()  { echo -e "${BLUE}[nerv]${NC} $*"; }
ok()    { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
fail()  { echo -e "${RED}[✗]${NC} $*"; exit 1; }

# --- Detect OS and Architecture ---
info "Detecting system..."
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) OS_NAME="macOS" ;;
    Linux)  OS_NAME="Linux" ;;
    *)      fail "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
    arm64|aarch64) ARCH_NAME="ARM64" ;;
    x86_64)        ARCH_NAME="x86_64" ;;
    *)             fail "Unsupported architecture: $ARCH" ;;
esac

ok "System: $OS_NAME $ARCH_NAME"

# --- Project root ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
info "Project root: $PROJECT_ROOT"

# --- Optional version manager (asdf) ---
# asdf >= 0.16 is a Go binary: it has no asdf.sh to source and a different CLI
# (`asdf set` instead of `asdf global`). We never bootstrap it ourselves; if it
# is already installed we use it, otherwise we print the official instructions.
ASDF_AVAILABLE=0
command -v asdf &>/dev/null && ASDF_AVAILABLE=1

asdf_pin_home() {
    # $1 = tool. Works with both asdf >= 0.16 (`set --home`) and older (`global`).
    if asdf set --help &>/dev/null; then
        asdf set --home "$1" latest
    else
        asdf global "$1" latest
    fi
}

# --- Check Rust ---
check_rust() {
    if command -v rustc &>/dev/null; then
        ok "Rust: $(rustc --version)"
    elif [ "$ASDF_AVAILABLE" = 1 ]; then
        info "Rust not found, installing via asdf..."
        asdf plugin add rust 2>/dev/null || true
        asdf install rust latest
        asdf_pin_home rust
        ok "Rust installed: $(rustc --version)"
    else
        fail "Rust not found. Install it from https://rustup.rs (review the installer first), then re-run."
    fi
}

# --- Check uv (Python package manager) ---
check_uv() {
    if command -v uv &>/dev/null; then
        ok "uv: $(uv --version)"
    elif command -v brew &>/dev/null; then
        info "uv not found, installing via Homebrew..."
        brew install uv
        ok "uv installed: $(uv --version)"
    else
        fail "uv not found. Install it per https://docs.astral.sh/uv/getting-started/installation/ (pipx install uv, or your package manager), then re-run."
    fi
}

# --- Check Python ---
check_python() {
    if command -v python3 &>/dev/null; then
        local version major minor
        version="$(python3 --version 2>&1)"
        major="$(echo "$version" | grep -oE '[0-9]+\.[0-9]+' | head -1 | cut -d. -f1)"
        minor="$(echo "$version" | grep -oE '[0-9]+\.[0-9]+' | head -1 | cut -d. -f2)"

        if [ "$major" -ge 3 ] && [ "$minor" -ge 11 ]; then
            ok "Python: $version"
            return
        else
            warn "Python $version found but >= 3.11 required"
        fi
    fi

    if [ "$ASDF_AVAILABLE" = 1 ]; then
        info "Installing Python via asdf..."
        asdf plugin add python 2>/dev/null || true
        asdf install python latest
        asdf_pin_home python
        ok "Python installed: $(python3 --version)"
    else
        # uv can fetch its own interpreter when `uv sync` runs.
        warn "System Python >= 3.11 not found; uv will download one during 'uv sync'"
    fi
}

# --- Ollama (opt-in) ---
# The default config runs every tier on claude-cli:opus; Ollama is only needed
# for `local:` models. Enable with NERV_SETUP_OLLAMA=1.
check_ollama() {
    if command -v ollama &>/dev/null; then
        ok "Ollama: $(ollama --version 2>&1 || echo 'installed')"
    else
        warn "Ollama not found. Install it from https://ollama.com/download (or: brew install ollama), then re-run."
        return 1
    fi
}

check_model() {
    local model="qwen2.5:3b"
    if ollama list 2>/dev/null | grep -q "$model"; then
        ok "Router model: $model (already pulled)"
    else
        info "Pulling router model: $model (this may take a few minutes)..."
        ollama pull "$model"
        ok "Router model pulled: $model"
    fi
}

# --- Build Rust ---
build_rust() {
    info "Building Rust gateway..."
    cd "$PROJECT_ROOT/core"
    cargo build 2>&1
    ok "Rust build successful"
    cd "$PROJECT_ROOT"
}

# --- Setup Python environment ---
setup_python() {
    info "Setting up Python brain environment..."
    cd "$PROJECT_ROOT/brain"
    uv sync 2>&1
    ok "Python dependencies installed"
    cd "$PROJECT_ROOT"
}

# --- Setup config ---
setup_config() {
    local config_file="$PROJECT_ROOT/config.yaml"
    if [ -f "$config_file" ]; then
        chmod 600 "$config_file"
        ok "config.yaml already exists (mode set to 600)"
    else
        info "Creating config.yaml from default template..."
        # The file will hold the bot token: create it owner-only.
        (umask 077 && cp "$PROJECT_ROOT/core/config.default.yaml" "$config_file")
        chmod 600 "$config_file"
        ok "config.yaml created (mode 600) — edit it to add your Telegram bot token and other settings"
    fi
}

# --- Main ---
echo ""
echo "╔══════════════════════════════════════╗"
echo "║     🧠 Nerv — Environment Setup     ║"
echo "╚══════════════════════════════════════╝"
echo ""

check_rust
check_uv
check_python
if [ "${NERV_SETUP_OLLAMA:-0}" = 1 ]; then
    check_ollama && check_model || warn "Skipping model pull (Ollama not available)"
else
    info "Skipping Ollama (only needed for local: models; set NERV_SETUP_OLLAMA=1 to set it up)"
fi
build_rust
setup_python
setup_config

echo ""
echo "═══════════════════════════════════════"
ok "Setup complete!"
echo ""
info "Next steps:"
echo "  1. Edit config.yaml to add your Telegram bot token"
echo "  2. Start Nerv: cd core && cargo run"
echo "  (Only if you switch models.tiers to local: models: run 'ollama serve' and re-run with NERV_SETUP_OLLAMA=1)"
echo ""
