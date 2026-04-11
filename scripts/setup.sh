#!/usr/bin/env bash
set -euo pipefail

# Nerv — Environment Bootstrap Script
# Detects and installs all required dependencies.
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

# --- Check / Install asdf (version manager) ---
check_version_manager() {
    if command -v asdf &>/dev/null; then
        ok "asdf: $(asdf version)"
        return
    fi

    info "No version manager found, installing asdf..."
    if [ "$OS" = "Darwin" ]; then
        if command -v brew &>/dev/null; then
            brew install asdf
            echo -e "\n. \"$(brew --prefix asdf)/libexec/asdf.sh\"" >> "$HOME/.zshrc"
            . "$(brew --prefix asdf)/libexec/asdf.sh"
        else
            info "Homebrew not found, installing asdf via git..."
            git clone https://github.com/asdf-vm/asdf.git "$HOME/.asdf" --branch v0.16.0
            echo -e "\n. \"$HOME/.asdf/asdf.sh\"" >> "$HOME/.zshrc"
            . "$HOME/.asdf/asdf.sh"
        fi
    else
        # Linux
        git clone https://github.com/asdf-vm/asdf.git "$HOME/.asdf" --branch v0.16.0
        echo -e "\n. \"$HOME/.asdf/asdf.sh\"" >> "$HOME/.bashrc"
        . "$HOME/.asdf/asdf.sh"
    fi
    ok "asdf installed: $(asdf version)"
}

# --- Check / Install Rust ---
check_rust() {
    if command -v rustc &>/dev/null; then
        ok "Rust: $(rustc --version)"
    else
        info "Rust not found, installing via asdf..."
        asdf plugin add rust 2>/dev/null || true
        asdf install rust latest
        asdf set --home rust latest
        ok "Rust installed: $(rustc --version)"
    fi
}

# --- Check / Install uv (Python package manager) ---
check_uv() {
    if command -v uv &>/dev/null; then
        ok "uv: $(uv --version)"
    else
        info "uv not found, installing..."
        curl -LsSf https://astral.sh/uv/install.sh | sh
        # Add to PATH for this session
        export PATH="$HOME/.local/bin:$PATH"
        ok "uv installed: $(uv --version)"
    fi
}

# --- Check / Install Python ---
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

    info "Installing Python via asdf..."
    asdf plugin add python 2>/dev/null || true
    asdf install python latest
    asdf set --home python latest
    ok "Python installed: $(python3 --version)"
}

# --- Check / Install Ollama ---
check_ollama() {
    if command -v ollama &>/dev/null; then
        ok "Ollama: $(ollama --version 2>&1 || echo 'installed')"
    else
        info "Ollama not found, installing..."
        if [ "$OS" = "Darwin" ]; then
            warn "Please install Ollama from https://ollama.ai/download"
            warn "Or: brew install ollama"
            return 1
        else
            curl -fsSL https://ollama.ai/install.sh | sh
            ok "Ollama installed"
        fi
    fi
}

# --- Check / Pull Router Model ---
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
        ok "config.yaml already exists"
    else
        info "Creating config.yaml from default template..."
        cp "$PROJECT_ROOT/core/config.default.yaml" "$config_file"
        ok "config.yaml created — edit it to add your Telegram bot token and other settings"
    fi
}

# --- Main ---
echo ""
echo "╔══════════════════════════════════════╗"
echo "║     🧠 Nerv — Environment Setup     ║"
echo "╚══════════════════════════════════════╝"
echo ""

check_version_manager
check_rust
check_uv
check_python
check_ollama && check_model || warn "Skipping model pull (Ollama not available)"
build_rust
setup_python
setup_config

echo ""
echo "═══════════════════════════════════════"
ok "Setup complete!"
echo ""
info "Next steps:"
echo "  1. Edit config.yaml to add your Telegram bot token"
echo "  2. Make sure Ollama is running: ollama serve"
echo "  3. Start Nerv: cd core && cargo run"
echo ""
