#!/usr/bin/env bash
set -euo pipefail

# Agentline Installer for macOS & Linux
# Usage: curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash
#        ./install.sh [--headless]

REPO="seven-tt/agentline"
REPO_URL="https://github.com/$REPO"

# ── parse args ───────────────────────────────────────────────────
INSTALL_TRAY=auto
for arg in "$@"; do
    case "$arg" in
        --headless) INSTALL_TRAY=false ;;
        --help|-h)
            echo "Usage: $0 [--headless]"
            echo "  --headless  Only install CLI (no tray app)"
            exit 0
            ;;
    esac
done

# ── detect platform ──────────────────────────────────────────────
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    darwin) PLATFORM="macOS" ;;
    linux)  PLATFORM="Linux" ;;
    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac

case "$OS-$ARCH" in
    darwin-arm64)   LABEL="mac-arm64" ;;
    darwin-x86_64)  LABEL="mac-x64" ;;
    linux-x86_64)   LABEL="linux-x64" ;;
    linux-aarch64)  LABEL="linux-arm64" ;;
    *)
        echo "Unsupported platform: $OS-$ARCH"
        exit 1
        ;;
esac

# ── decide whether to install tray ───────────────────────────────
if [[ "$INSTALL_TRAY" == "auto" ]]; then
    if [[ "$OS" == "linux" ]] && [[ -z "${DISPLAY:-}" ]] && [[ -z "${WAYLAND_DISPLAY:-}" ]] && [[ "${XDG_SESSION_TYPE:-}" != "x11" ]] && [[ "${XDG_SESSION_TYPE:-}" != "wayland" ]]; then
        INSTALL_TRAY=false
        echo "No graphical display detected, installing headless (CLI only)."
    else
        INSTALL_TRAY=true
    fi
fi

# ── install dir ──────────────────────────────────────────────────
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$INSTALL_DIR"

# ── helper: get latest tag ───────────────────────────────────────
get_latest_tag() {
    if command -v curl &>/dev/null; then
        curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/'
    fi
}

# ── helper: download a single binary ─────────────────────────────
download_binary() {
    local bin_name="$1" tag="$2"
    local version="${tag#v}"
    local asset_name="${bin_name}-${version}-${LABEL}"
    local url="$REPO_URL/releases/download/${tag}/${asset_name}"

    echo "Downloading $asset_name ..."
    if curl -fsSL -o "$INSTALL_DIR/$bin_name" "$url"; then
        chmod +x "$INSTALL_DIR/$bin_name"
        echo "Installed $bin_name $version -> $INSTALL_DIR/$bin_name"
        return 0
    else
        return 1
    fi
}

# ── helper: download and install macOS .dmg ──────────────────────
download_macos_dmg() {
    local tag="$1"
    local version="${tag#v}"
    local dmg_name="agentline-tray-${version}-${LABEL}.dmg"
    local url="$REPO_URL/releases/download/${tag}/${dmg_name}"
    local app_dest="/Applications/AgentlineTray.app"

    echo "Downloading $dmg_name ..."
    local tmpdmg
    tmpdmg="$(mktemp).dmg"
    if curl -fsSL -o "$tmpdmg" "$url"; then
        local mount_dir
        mount_dir="$(mktemp -d)"
        hdiutil attach "$tmpdmg" -mountpoint "$mount_dir" -nobrowse -quiet
        rm -rf "$app_dest"
        cp -R "$mount_dir/AgentlineTray.app" /Applications/
        hdiutil detach "$mount_dir" -quiet
        rm -f "$tmpdmg"
        echo "Installed AgentlineTray.app -> $app_dest"
        return 0
    else
        rm -f "$tmpdmg"
        return 1
    fi
}

# ── helper: download and install Linux .deb ──────────────────────
download_linux_deb() {
    local tag="$1"
    local version="${tag#v}"
    local deb_name="agentline-tray-${version}-${LABEL}.deb"
    local url="$REPO_URL/releases/download/${tag}/${deb_name}"

    echo "Downloading $deb_name ..."
    local tmpdeb
    tmpdeb="$(mktemp).deb"
    if curl -fsSL -o "$tmpdeb" "$url"; then
        if command -v sudo &>/dev/null; then
            sudo dpkg -i "$tmpdeb" || sudo apt-get install -f -y
        else
            dpkg -i "$tmpdeb" || apt-get install -f -y
        fi
        rm -f "$tmpdeb"
        echo "Installed agentline + agentline-tray via .deb"
        return 0
    else
        rm -f "$tmpdeb"
        return 1
    fi
}

# ── try downloading prebuilt binaries ────────────────────────────
LATEST_TAG="$(get_latest_tag)"
NEED_BUILD=false

if [[ -n "$LATEST_TAG" ]]; then
    if [[ "$INSTALL_TRAY" == "true" ]]; then
        if [[ "$OS" == "darwin" ]]; then
            if ! download_macos_dmg "$LATEST_TAG"; then
                NEED_BUILD=true
            fi
        elif [[ "$OS" == "linux" ]]; then
            if ! download_linux_deb "$LATEST_TAG"; then
                NEED_BUILD=true
            fi
        fi
    else
        if ! download_binary "agentline" "$LATEST_TAG"; then
            NEED_BUILD=true
        fi
    fi
else
    NEED_BUILD=true
fi

# ── fallback: build from source ──────────────────────────────────
if [[ "$NEED_BUILD" == "true" ]]; then
    echo ""
    echo "Falling back to building from source ..."

    if ! command -v cargo &>/dev/null; then
        echo "Rust toolchain not found."
        echo "Install it first:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi

    RUST_VERSION="$(rustc --version | awk '{print $2}')"
    echo "Rust $RUST_VERSION detected"

    if [[ -f "Cargo.toml" && -d "crates" ]]; then
        SOURCE_DIR="$(pwd)"
        echo "Using current directory as source"
    else
        SOURCE_DIR="$(mktemp -d)/agentline"
        echo "Cloning $REPO_URL ..."
        git clone --depth 1 "$REPO_URL.git" "$SOURCE_DIR"
        cd "$SOURCE_DIR"
    fi

    if [[ ! -f "$INSTALL_DIR/agentline" ]]; then
        echo "Building agentline ..."
        cargo build --release --package agentline
        cp "target/release/agentline" "$INSTALL_DIR/agentline"
        chmod +x "$INSTALL_DIR/agentline"
        echo "Installed agentline -> $INSTALL_DIR/agentline"
    fi

    if [[ "$INSTALL_TRAY" == "true" && ! -f "$INSTALL_DIR/agentline-tray" ]]; then
        echo "Building agentline-tray ..."
        cargo build --release --package agentline-tray
        cp "target/release/agentline-tray" "$INSTALL_DIR/agentline-tray"
        chmod +x "$INSTALL_DIR/agentline-tray"
        echo "Installed agentline-tray -> $INSTALL_DIR/agentline-tray"
    fi
fi

# ── add to PATH if needed (only for headless install to ~/.local/bin) ─
if [[ "$INSTALL_TRAY" != "true" ]] && [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    SHELL_RC=""
    case "$(basename "${SHELL:-bash}")" in
        bash) SHELL_RC="$HOME/.bashrc" ;;
        zsh)  SHELL_RC="$HOME/.zshrc" ;;
    esac
    if [[ -n "$SHELL_RC" ]]; then
        echo "export PATH=\"\$PATH:$INSTALL_DIR\"" >> "$SHELL_RC"
        echo "Added $INSTALL_DIR to PATH in $SHELL_RC"
    fi
fi

# ── init config ──────────────────────────────────────────────────
CONFIG_DIR="$HOME/.agentline"
CONFIG_FILE="$CONFIG_DIR/config.toml"

if [[ ! -f "$CONFIG_FILE" ]]; then
    mkdir -p "$CONFIG_DIR"
    EXAMPLE=""
    if [[ -f "config.example.toml" ]]; then
        EXAMPLE="config.example.toml"
    elif [[ -n "${SOURCE_DIR:-}" && -f "$SOURCE_DIR/config.example.toml" ]]; then
        EXAMPLE="$SOURCE_DIR/config.example.toml"
    fi
    if [[ -n "$EXAMPLE" ]]; then
        cp "$EXAMPLE" "$CONFIG_FILE"
        echo "Created default config at $CONFIG_FILE"
        echo "  Edit it to set IM credentials and agent backend, then run 'agentline'."
    fi
else
    echo "Config already exists at $CONFIG_FILE"
fi

# ── post-install hints ───────────────────────────────────────────
echo ""
echo "Installation complete!"
if [[ "$INSTALL_TRAY" == "true" ]]; then
    if [[ "$OS" == "darwin" ]]; then
        echo "  App:    /Applications/AgentlineTray.app"
        echo "  CLI:    /Applications/AgentlineTray.app/Contents/MacOS/agentline"
    else
        echo "  CLI:    /usr/bin/agentline"
        echo "  Tray:   /usr/bin/agentline-tray"
    fi
else
    echo "  Binary: $INSTALL_DIR/agentline"
fi
echo "  Config: ${CONFIG_FILE:-$CONFIG_DIR/config.toml}"
