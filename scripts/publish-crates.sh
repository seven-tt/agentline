#!/usr/bin/env bash
set -euo pipefail

REPO="https://github.com/seven-tt/agentline.git"
WORK_DIR="/tmp/agentline-publish"
DRY_RUN="${DRY_RUN:-}"
SLEEP="${PUBLISH_SLEEP:-20}"

# ── resolve tag ─────────────────────────────────────────────────

TAG="${1:-}"
if [[ -z "$TAG" ]]; then
  echo "Usage: $0 <tag>        e.g. $0 v1.0.1"
  echo "       $0 latest       auto-detect latest tag"
  exit 1
fi

# ── helpers ──────────────────────────────────────────────────────

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }

# ── clone from GitHub tag ───────────────────────────────────────

echo "╔══════════════════════════════════════════════╗"
echo "║   Agentline → crates.io publish             ║"
echo "╚══════════════════════════════════════════════╝"
echo ""

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"

if [[ "$TAG" == "latest" ]]; then
  echo "==> Detecting latest tag from GitHub ..."
  TAG=$(git ls-remote --tags --sort=-v:refname "$REPO" 'v*' \
    | head -1 | sed 's|.*refs/tags/||; s|\^{}||')
  if [[ -z "$TAG" ]]; then
    red "ERROR: no tags found in $REPO"
    exit 1
  fi
  green "    Latest tag: $TAG"
fi

echo "==> Cloning $REPO @ $TAG ..."
git clone --depth 1 --branch "$TAG" "$REPO" "$WORK_DIR/agentline"
cd "$WORK_DIR/agentline"
green "    Checked out $TAG ($(git rev-parse --short HEAD))"
echo ""

if [[ -n "$DRY_RUN" ]]; then
  yellow "🔍 DRY RUN MODE (no actual publish)"
  echo ""
fi

# ── build dashboard (cli crate needs it at compile time) ────────

echo "==> Building dashboard ..."
cd "$WORK_DIR/agentline/web/dashboard"
npm install --silent
npm run build
mkdir -p "$WORK_DIR/agentline/crates/cli/templates"
cp dist/index.html "$WORK_DIR/agentline/crates/cli/templates/dashboard.html"
cd "$WORK_DIR/agentline"
green "    dashboard.html ready"
echo ""

# ── publish order (topological sort by internal deps) ───────────
# L0 (no internal deps): permission, telegramify-markdown
# L1 (depends on L0):    bridge (← permission)
# L2 (depends on L1):    agent-acp, im-core, transport (← bridge)
# L3 (depends on L2):    agent-*, im-*, transport-iroh
# L4 (depends on L3):    cli (agentline), tray

CRATES=(
  "crates/permission"
  "crates/telegramify"

  "crates/bridge"

  "crates/agent/acp"
  "crates/im/core"
  "crates/transport/core"

  "crates/agent/claude-code"
  "crates/agent/codex"
  "crates/agent/gemini"
  "crates/agent/hermes"
  "crates/agent/kimi"
  "crates/agent/kiro"
  "crates/agent/opencode"
  "crates/agent/qoder"

  "crates/im/dingtalk"
  "crates/im/feishu"
  "crates/im/telegram"
  "crates/im/wechat"

  "crates/transport/iroh"

  "crates/cli"
  "crates/tray"
)

# ── publish each crate ──────────────────────────────────────────

publish_crate() {
  local dir="$1"
  local name
  name=$(grep '^name' "$dir/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
  local ver
  ver=$(grep '^version' "Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

  printf "  📦 %-30s v%s " "$name" "$ver"

  if [[ -n "$DRY_RUN" ]]; then
    if cargo publish --dry-run -p "$name" --allow-dirty 2>/dev/null; then
      green "✓ (dry-run)"
    else
      red "✗ (dry-run failed)"
      return 1
    fi
  else
    local output
    if output=$(cargo publish -p "$name" 2>&1); then
      green "✓"
    else
      if echo "$output" | grep -q "already uploaded"; then
        yellow "⊘ (already published)"
      else
        red "✗"
        echo "$output" >&2
        return 1
      fi
    fi
  fi
}

echo "==> Publishing ${#CRATES[@]} crates from $TAG ..."
echo ""

FAILED=()
for crate_dir in "${CRATES[@]}"; do
  if ! publish_crate "$crate_dir"; then
    FAILED+=("$crate_dir")
    red "    ⚠ Stopping: fix the above error before continuing"
    break
  fi
  if [[ -z "$DRY_RUN" ]]; then
    sleep "$SLEEP"
  fi
done

echo ""
if [[ ${#FAILED[@]} -eq 0 ]]; then
  green "✅ All ${#CRATES[@]} crates published from $TAG!"
else
  red "❌ Failed crates: ${FAILED[*]}"
  echo "    Work dir preserved at: $WORK_DIR"
  exit 1
fi

# cleanup
rm -rf "$WORK_DIR"
echo "    Cleaned up $WORK_DIR"
