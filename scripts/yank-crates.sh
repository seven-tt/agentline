#!/usr/bin/env bash
set -euo pipefail

# Yank all agentline crates for a given version from crates.io.
# Usage: ./scripts/yank-crates.sh v1.0.2

TAG="${1:-}"
if [[ -z "$TAG" ]]; then
  echo "Usage: $0 <tag>   e.g. $0 v1.0.2"
  exit 1
fi

VERSION="${TAG#v}"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }

CRATES=(
  agentline-permission
  telegramify-markdown
  agentline-bridge
  agentline-agent-acp
  agentline-im-core
  agentline-transport
  agentline-agent-claude-code
  agentline-agent-codex
  agentline-agent-gemini
  agentline-agent-hermes
  agentline-agent-kimi
  agentline-agent-kiro
  agentline-agent-opencode
  agentline-agent-qoder
  agentline-im-dingtalk
  agentline-im-feishu
  agentline-im-telegram
  agentline-im-wechat
  agentline-transport-iroh
  agentline
  agentline-tray
)

echo "Yanking v${VERSION} for ${#CRATES[@]} crates ..."
echo ""

for name in "${CRATES[@]}"; do
  printf "  📦 %-35s " "$name"
  output=$(cargo yank "$name" --version "$VERSION" 2>&1) && {
    green "✓ yanked"
  } || {
    if echo "$output" | grep -qi "does not have a version"; then
      yellow "⊘ not published"
    elif echo "$output" | grep -qi "already yanked"; then
      yellow "⊘ already yanked"
    else
      red "✗ $output"
    fi
  }
done

echo ""
green "Done. Yanked versions are hidden from cargo add/install but not deleted."
