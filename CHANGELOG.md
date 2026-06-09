# Changelog

All notable changes to this project are documented here. Format follows
[keep a changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Workspace scaffolding: `agentline-bridge` (neutral trait + runtime), `agentline`
  (CLI binary), per-platform IM and agent adapter crates.
- `agentline-im-wechat`: Tencent iLink (officially-released personal WeChat
  Bot HTTP API). Long-poll `/v1.0/getupdates`, QR-code login, cursor
  persistence.
- `agentline-im-dingtalk`: DingTalk Stream API (raw WebSocket, no SDK).
  HTTP handshake → WS connect → DataFrame ACK loop with auto-reconnect.
- `agentline-agent-acp`: generic ACP transport. Drives any agent that
  speaks ACP over stdio. Built around the official `agent-client-protocol`
  crate (pinned to 0.10.x — the current 0.13.x closure-based API hides the
  child-spawning we need for env control).
- `agentline-agent-claude-code`: Claude Code adapter. Thin wrapper around
  `agentline-agent-acp` that spawns `npx @zed-industries/claude-code-acp`,
  scrubs the `CLAUDECODE`-family env vars (nested-session detection), and
  optionally injects `~/.claude/settings.json` env into the process.
- `agentline-agent-kimi`: Kimi Code CLI adapter. Thin wrapper around
  `agentline-agent-acp` that spawns `kimi acp` (Moonshot AI's official CLI,
  natively ACP-compatible). Requires `pip install kimi-cli && kimi /login`
  out-of-band first.
- `agentline-agent-qoder`: Qoder CLI adapter. Thin wrapper around
  `agentline-agent-acp` that spawns `qodercli --acp` (Qoder's official CLI,
  natively ACP-compatible per <https://docs.qoder.com/en/cli/acp>). Auth
  either via `qodercli login` or by setting `QODER_PERSONAL_ACCESS_TOKEN`.
- `agentline-agent-opencode`: OpenCode CLI adapter ([sst/opencode]).
  Thin wrapper around `agentline-agent-acp` that spawns `opencode acp` —
  OpenCode uses the standard `@agentclientprotocol/sdk` under the hood.
  Auth via `opencode auth login`.
- `agentline-agent-kiro`: Kiro CLI adapter (AWS Kiro IDE's headless
  companion, [docs](https://kiro.dev/docs/cli/acp/)). Thin wrapper around
  `agentline-agent-acp` that spawns `kiro-cli acp [--agent <name>]`.
  Install via `curl -fsSL https://cli.kiro.dev/install | bash`. Note:
  Kiro's docs warn that IDEs / launchers may not inherit your shell's
  PATH — use the absolute path (typically `~/.local/bin/kiro-cli`) in
  `[agent.kiro] command = ...` if `kiro-cli` isn't found.
- `agentline-agent-gemini`: Google Gemini CLI adapter
  ([google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli)).
  Thin wrapper around `agentline-agent-acp` that spawns `gemini --acp` —
  Gemini CLI uses the standard `@agentclientprotocol/sdk` under the hood.
  Install via `npm install -g @google/gemini-cli` or `brew install gemini-cli`,
  then sign in once interactively.
- `agentline service {install,uninstall,status,logs}`: macOS launchd
  integration for running the bridge as a background daemon. Writes
  `~/Library/LaunchAgents/com.agentline.daemon.plist`, bootstraps via
  `launchctl`, redirects stdout/stderr to `~/.agentline/agentline.log`.
- `agentline-tray` (new crate): macOS menu-bar controller for the daemon.
  Built on `tray-icon` + `tao`. Hides itself from the dock via
  `NSApplication.setActivationPolicy(.accessory)`. Menu: status / open
  dashboard / open log / open config / restart daemon / quit.
  `agentline-tray install` writes its own launchd plist so the menu-bar
  icon auto-starts at login. Does NOT embed the bridge — purely a
  monitor/controller. Icon is a template-image silhouette (auto-tints
  for light/dark menu bars), rasterized from an embedded SVG via
  `resvg` at 64×64.
- Embedded web dashboard: daemon now serves a static HTML page at
  `http://127.0.0.1:7681` (configurable via `[web]` section). Endpoints:
  `/api/status`, `/api/logs`, `/api/login/{start,cancel,status,qr.png}`.
  The QR-code login flow runs the page inline — no Preview pop-up
  needed. Daemon stays alive even when the bridge can't start (e.g.
  missing token) so the user can recover via the dashboard.

[sst/opencode]: https://github.com/sst/opencode
- `agentline-agent-codex`: OpenAI Codex CLI adapter. Does NOT use the
  shared ACP transport — Codex speaks its own JSON-RPC "app-server"
  protocol. Uses the official `codex-app-server-sdk` to spawn
  `codex app-server` and maps Codex's `ThreadEvent` / `ThreadItem` stream
  into our neutral `AgentUpdate`. Requires `codex` on PATH and prior
  `codex login`. Configurable `sandbox_mode` / `approval_mode`; defaults
  to `workspace-write` + `never` for a yolo-style experience.
- Bridge runtime: per-prompt streaming, throttled flush, slash commands
  (`/cd /new /stop /yolo /safe /help`), permission round-trip, typing
  indicator.

### Notes
- Placeholder crates `agentline-agent-codex` and `agentline-agent-kimi`
  are marked `publish = false` until they have real implementations.
