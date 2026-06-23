#!/usr/bin/env bash
set -euo pipefail

# Delete agentline crates from crates.io (reverse dependency order).
# Only works within 72 hours of publish and with no external reverse deps.
# Usage: ./scripts/delete-crates.sh v1.0.2

TAG="${1:-}"
if [[ -z "$TAG" ]]; then
  echo "Usage: $0 <tag>   e.g. $0 v1.0.2"
  exit 1
fi

VERSION="${TAG#v}"

red()   { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }

# Reverse dependency order: delete dependents first, then their deps
CRATES=(
  agentline-im-core
  agentline-agent-acp
  agentline-bridge
  telegramify-markdown
  agentline-permission
)

TOKEN=$(cargo config get registries.crates-io.token 2>/dev/null \
  | sed 's/.*"\(.*\)".*/\1/' || true)

if [[ -z "$TOKEN" ]]; then
  TOKEN=$(grep -A1 '\[registries.crates-io\]' ~/.cargo/credentials.toml 2>/dev/null \
    | grep token | sed 's/.*"\(.*\)".*/\1/' || true)
fi

if [[ -z "$TOKEN" ]]; then
  TOKEN=$(grep 'token' ~/.cargo/credentials.toml 2>/dev/null \
    | head -1 | sed 's/.*"\(.*\)".*/\1/' || true)
fi

if [[ -z "$TOKEN" ]]; then
  red "ERROR: could not find crates.io token. Run 'cargo login' first."
  exit 1
fi

echo "Deleting v${VERSION} for ${#CRATES[@]} crates (reverse dep order) ..."
echo ""

for name in "${CRATES[@]}"; do
  printf "  🗑  %-35s " "$name"
  status=$(curl -s -o /tmp/crate-delete-resp.txt -w "%{http_code}" \
    -X DELETE \
    -H "Authorization: $TOKEN" \
    -H "User-Agent: agentline-publish-script (seven-tt)" \
    "https://crates.io/api/v1/crates/${name}/${VERSION}")

  case "$status" in
    200) green "✓ deleted" ;;
    404) yellow "⊘ not found" ;;
    *)
      msg=$(cat /tmp/crate-delete-resp.txt 2>/dev/null)
      red "✗ HTTP $status — $msg"
      ;;
  esac
  sleep 2
done

echo ""
green "Done."
