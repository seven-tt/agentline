#!/usr/bin/env bash
set -euo pipefail

# Agentline Installer for macOS & Linux
# Usage: curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash
#        ./install.sh [--cli|--tray]

REPO_URL="https://github.com/seven-tt/agentline"
INSTALL_MODE="${1:-cli}"

if [[ "$INSTALL_MODE" != "cli" && "$INSTALL_MODE" != "tray" ]]; then
    echo "Usage: $0 [--cli|--tray]"
    echo "  --cli   Install headless CLI (default)"
    echo "  --tray  Install menu-bar / system-tray app (macOS recommended)"
    exit 1
fi

# ── detect platform ──────────────────────────────────────────────
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    darwin) PLATFORM="macOS" ;;
    linux)  PLATFORM="Linux" ;;
    *)
        echo "❌ Unsupported OS: $OS"
        echo "   This installer supports macOS and Linux only."
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64)  TARGET="x86_64" ;;
    arm64|aarch64) TARGET="aarch64" ;;
    *)
        echo "❌ Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

# ── tray availability ────────────────────────────────────────────
if [[ "$INSTALL_MODE" == "tray" && "$PLATFORM" != "macOS" ]]; then
    echo "⚠️  Tray mode is primarily designed for macOS."
    echo "   On Linux the tray may have limited functionality."
    read -r -p "   Continue anyway? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || exit 0
fi

# ── check Rust ───────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
    echo "❌ Rust toolchain not found."
    echo "   Install it first:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

RUST_VERSION="$(rustc --version | awk '{print $2}')"
echo "✅ Rust $RUST_VERSION detected"

# ── get source ───────────────────────────────────────────────────
if [[ -f "Cargo.toml" && -d "crates" ]]; then
    SOURCE_DIR="$(pwd)"
    echo "📁 Using current directory as source"
else
    SOURCE_DIR="$(mktemp -d)/agentline"
    echo "📥 Cloning $REPO_URL …"
    git clone --depth 1 "$REPO_URL.git" "$SOURCE_DIR"
    cd "$SOURCE_DIR"
fi

# ── build & install ──────────────────────────────────────────────
if [[ "$INSTALL_MODE" == "tray" ]]; then
    echo "🔨 Building agentline-tray …"
    cargo build --release --package agentline-tray
    BIN="target/release/agentline-tray"
    BIN_NAME="agentline-tray"
else
    echo "🔨 Building agentline …"
    cargo build --release --package agentline
    BIN="target/release/agentline"
    BIN_NAME="agentline"
fi

# ── install binary ───────────────────────────────────────────────
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$INSTALL_DIR"
cp "$BIN" "$INSTALL_DIR/$BIN_NAME"
chmod +x "$INSTALL_DIR/$BIN_NAME"

# add to PATH if needed
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    SHELL_RC=""
    case "$(basename "$SHELL")" in
        bash) SHELL_RC="$HOME/.bashrc" ;;
        zsh)  SHELL_RC="$HOME/.zshrc" ;;
    esac
    if [[ -n "$SHELL_RC" ]]; then
        echo "export PATH=\"\$PATH:$INSTALL_DIR\"" >> "$SHELL_RC"
        echo "📝 Added $INSTALL_DIR to PATH in $SHELL_RC"
    fi
fi

echo "✅ Installed $BIN_NAME → $INSTALL_DIR/$BIN_NAME"

# ── init config ──────────────────────────────────────────────────
CONFIG_DIR="$HOME/.agentline"
CONFIG_FILE="$CONFIG_DIR/config.toml"

if [[ ! -f "$CONFIG_FILE" ]]; then
    mkdir -p "$CONFIG_DIR"
    cp "$SOURCE_DIR/config.example.toml" "$CONFIG_FILE"
    echo "✨ Created default config at $CONFIG_FILE"
    echo "   Edit it to set IM credentials and agent backend, then run '$BIN_NAME'."
else
    echo "📄 Config already exists at $CONFIG_FILE"
fi

# ── macOS service / tray setup ───────────────────────────────────
if [[ "$PLATFORM" == "macOS" ]]; then
    if [[ "$INSTALL_MODE" == "cli" ]]; then
        echo ""
        echo "💡 To run agentline as a background service:"
        echo "   $BIN_NAME service install"
        echo "   $BIN_NAME service status"
    else
        echo ""
        echo "💡 To start the tray app:"
        echo "   $BIN_NAME"
        echo ""
        echo "   To auto-start on login, add it to System Settings → Login Items."
    fi
fi

echo ""
echo "🎉 Installation complete!"
echo "   Binary: $INSTALL_DIR/$BIN_NAME"
echo "   Config: $CONFIG_FILE"
