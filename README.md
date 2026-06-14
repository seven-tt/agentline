<p align="center">
  <h1 align="center">Agentline</h1>
  <p align="center">
    <strong>Connect Any IM to Any Coding Agent — In One Binary</strong>
  </p>
  <p align="center">
    <a href="https://github.com/seven-tt/agentline/actions/workflows/ci.yml"><img src="https://github.com/seven-tt/agentline/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License"></a>
    <a href="#"><img src="https://img.shields.io/badge/rust-1.89+-orange" alt="Rust"></a>
  </p>
</p>

[中文文档](README_zh-CN.md)

---

Agentline is a high-performance Rust bridge that turns any instant messaging platform into a fully-featured coding agent interface. Deploy once, and your team gets AI-powered development assistance directly in the chat tools they already use — no new apps, no context switching.

```
 IM Channels            Core             Agent Backends
+------------+     +-----------+     +------------------+
|  WeChat    |     |           |     |  Claude Code     |
|  DingTalk  +---->+  Bridge   +---->+  Gemini CLI      |
|  Feishu    |     |  (Actor)  |     |  Kimi / Qoder    |
|  Telegram  |     |           |     |  Hermes / Kiro   |
+------------+     +-----------+     +------------------+
 4 IM adapters                        9 backends + custom
 WebSocket / polling                  ACP protocol
```

## Why Agentline?

- **Zero friction** — your team already uses WeChat / DingTalk / Feishu / Telegram; just add a bot
- **Agent-agnostic** — swap between Claude Code, Gemini, Kimi, Codex, or any ACP-compatible agent with one config change
- **Actor-based runtime** — lock-free message routing via Tokio channels; the bridge actor owns all mutable state exclusively
- **Production-grade** — crash recovery, orphan process cleanup, session isolation, process tree management
- **Self-hosted & private** — runs on your own infrastructure, no data leaves your network
- **Single binary** — one `cargo install`, no runtime dependencies

## Supported Platforms

### IM Adapters

| Platform | Protocol | Status |
|----------|----------|--------|
| **WeChat** | iLink Personal Bot API | Stable |
| **DingTalk** | Stream API (WebSocket) | Stable |
| **Feishu (Lark)** | WebSocket long-connection + REST API | Stable |
| **Telegram** | Bot API (long polling) | Stable |

### Agent Backends

| Agent | Protocol | Notes |
|-------|----------|-------|
| **Claude Code** | ACP | Permission delegation, elicitation |
| **Gemini CLI** | ACP | Google's gemini-cli |
| **Kimi Code** | ACP | Moonshot's kimi-cli |
| **Qoder** | ACP | Personal access token support |
| **OpenCode** | ACP | sst/opencode |
| **Hermes** | ACP | Nous Research, OAuth login |
| **Kiro** | ACP | AWS Kiro CLI, multi-agent configs |
| **OpenAI Codex** | JSON-RPC | Via official codex-app-server-sdk |
| **Any ACP agent** | ACP | Generic adapter — bring your own |

## Quick Start

Runs on **macOS**, **Linux**, and **Windows**.

### One-line Install (recommended)

```bash
# macOS / Linux — installs tray app + CLI
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash

# Windows (PowerShell) — downloads and runs the installer
irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex

# Headless only (servers without GUI)
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash -s -- --headless
```

On macOS, the installer mounts a `.dmg` and copies `AgentlineTray.app` to `/Applications/`. On Linux, it downloads a `.deb` package and installs via `dpkg`. On Windows, it downloads and runs an Inno Setup installer. Linux auto-detects headless environments and falls back to CLI-only.

### Configure and Run

```bash
agentline                            # auto-creates ~/.agentline/config.toml
$EDITOR ~/.agentline/config.toml     # set IM credentials + choose agent
agentline                            # start bridging
```

For WeChat, run `agentline login` first to scan the QR code.

## Architecture

```
crates/
├── bridge/          Core runtime: actor, message routing, permissions, i18n
├── cli/             Binary entry point, config, service management, web dashboard
├── im/
│   ├── core/        Shared IM traits (ImAdapter, InboundHandler)
│   ├── wechat/      iLink Bot API adapter
│   ├── dingtalk/    DingTalk Stream (WebSocket) adapter
│   ├── feishu/      Feishu WebSocket long-connection adapter
│   └── telegram/    Telegram Bot API (long-polling) adapter
└── agent/
    ├── acp/         Generic ACP transport (shared by 8 backends)
    ├── claude-code/ Claude Code = ACP + env/settings plumbing
    ├── kimi/        Kimi = ACP + CLI launch config
    ├── qoder/       Qoder = ACP + personal access token
    ├── opencode/    OpenCode = ACP + CLI launch config
    ├── hermes/      Hermes = ACP + OAuth login
    ├── kiro/        Kiro = ACP + multi-agent routing
    ├── gemini/      Gemini = ACP + CLI launch config
    └── codex/       Codex = JSON-RPC via official SDK
```

### Bridge Runtime

The bridge uses an **actor model**: a lightweight `Bridge` handle (clone-able, holds only a channel sender) forwards commands to a single `BridgeActor` that exclusively owns all mutable state — zero `Mutex`, zero `lock().await`.

```
  IM adapters ──┐
                ├──▶ Bridge (handle)  ──mpsc──▶  BridgeActor (owns state)
  CLI / tests ──┘    Clone, Send                 tokio::select! loop
```

The actor multiplexes IM inbound messages and internal commands in a `tokio::select!` loop:

1. **Message arrives** from IM → parsed → routed to session
2. **Session management** → lazy session creation with per-agent working directory
3. **Prompt dispatch** → forwarded to agent via `AgentBackend::prompt()`
4. **Streaming response** → throttled, formatted per IM platform, streamed back
5. **Permission flow** → agent requests → interactive prompt in chat → user replies → resolved

### Design Principles

- **ACP is a protocol, not a product.** The transport layer (`agentline-agent-acp`) handles all protocol mechanics. Adding a new ACP agent is ~70 lines of config wrapper.
- **IM adapters are self-contained.** Each adapter implements `ImAdapter` + `InboundHandler` and knows nothing about other adapters or agent backends.
- **Process lifecycle is OS-level.** On Unix, `setsid` creates isolated sessions and `kill_session` reaps entire trees; on Windows, `sysinfo` walks the process tree. `kill_on_drop` provides belt-and-suspenders safety.

## Key Features

### Message Routing
- Paragraph-aware throttling (buffer to boundary / 600 chars / 2s)
- Per-session state isolation with configurable idle timeout
- Multi-session support with `/sessions` management

### Permission & Security
- Interactive permission delegation: agent asks → user confirms in chat (`y` / `n` / `s` for session-grant)
- `/yolo` mode for trusted sessions, auto-resets on new session
- User whitelist per IM platform

### Reliability
- **Crash recovery**: agent process dies → automatic respawn (up to 5 retries, 2s cooldown)
- **Orphan cleanup**: full process tree kill on shutdown
- **PID tracking**: stale processes from previous crashes cleaned on startup
- **Single-instance lock**: prevents duplicate daemons

### Operations
- Background service via launchd (macOS) with auto-start/restart
- Built-in web dashboard (status, logs, WeChat QR login)
- Cross-platform system tray app
- Structured logging with configurable levels
- Proxy injection for LAN/corporate environments (RFC-1918 auto-bypass)

### Internationalization
- Chinese / English, runtime-switchable
- All user-facing strings externalized to YAML locale files
- Web dashboard localized via `vue-i18n`

## Configuration

```toml
[im.feishu]
enable = true
app_id = "cli_xxxxx"
app_secret = "xxxxx"
allowed_users = []                    # empty = allow all

[im.telegram]
enable = false
bot_token = ""
allowed_users = []

[agent]
backend = "claude-code"               # 9 built-in options + generic "acp"

[bridge]
default_cwd = ""                      # empty = auto-isolate per agent
locale = "zh-CN"                      # "zh-CN" | "en"
session_idle_timeout_secs = 7200      # auto-reset after 2h idle

[web]
enable = true
bind = "127.0.0.1:7681"

[proxy]
http = ""                             # injected into all agent subprocesses
```

See [`config.example.toml`](config.example.toml) for the full reference.

## Deployment

```bash
# Foreground
agentline

# Background service (macOS)
agentline service install      # launchd plist, auto-start on boot
agentline service status
agentline service logs --tail

# System tray
agentline-tray
```

## Building from Source

```bash
git clone https://github.com/seven-tt/agentline
cd agentline
cargo build --release --bin agentline        # CLI
cargo build --release --bin agentline-tray   # System tray
```

## Acknowledgements

- [agent-client-protocol](https://github.com/agentclientprotocol/rust-sdk) — ACP Rust SDK
- [claude-code-acp](https://github.com/zed-industries/claude-code-acp) — Claude Code's ACP bridge by Zed Industries
- [acp-cli](https://github.com/motosan-dev/acp-cli) — architecture inspiration
- [openclaw-weixin](https://github.com/hao-ji-xing/openclaw-weixin) — iLink Bot API protocol reference

## License

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE).

---

<p align="center">
  <sub>Built with Rust. Designed for teams who ship.</sub>
</p>
