<p align="center">
  <h1 align="center">Agentline</h1>
  <p align="center">
    <strong>Connect Any IM to Any Coding Agent — In One Binary</strong>
  </p>
  <p align="center">
    <a href="https://github.com/seven-tt/agentline/actions/workflows/ci.yml"><img src="https://github.com/seven-tt/agentline/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License"></a>
    <a href="#"><img src="https://img.shields.io/badge/rust-1.85+-orange" alt="Rust"></a>
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
|  Feishu    |     |           |     |  Kimi / Qoder    |
|  Telegram  |     |           |     |  Hermes / Kiro   |
+------------+     +-----------+     +------------------+
 each has own                         9 backends + custom
 enable flag                          ACP adapter
```

## Why Agentline?

- **Zero friction adoption** — your team uses WeChat / DingTalk / Feishu daily; just add a bot, no new tool to learn
- **Agent-agnostic** — swap between Claude Code, Gemini, Kimi, Codex, or any ACP-compatible agent with one config change
- **Production-grade reliability** — automatic crash recovery, orphan process cleanup, session isolation, and process tree management
- **Self-hosted & private** — runs on your own infrastructure, no data leaves your network
- **Single binary, minimal footprint** — one `cargo install`, no runtime dependencies, ~10MB binary

## Supported Platforms

### IM Adapters

| Platform | Protocol | Status |
|----------|----------|--------|
| **WeChat** | iLink Personal Bot API | Stable |
| **DingTalk** | Stream API (native WebSocket) | Stable |
| **Feishu (Lark)** | Event Subscription + Bot API | Stable |
| **Telegram** | Bot API (Long Polling) | Stable |

### Agent Backends

| Agent | Protocol | Notes |
|-------|----------|-------|
| **Claude Code** | ACP | Full support incl. permission delegation, elicitation |
| **Gemini CLI** | ACP | Google's gemini-cli, native ACP |
| **Kimi Code** | ACP | Moonshot's kimi-cli, native ACP |
| **Qoder** | ACP | Qoder CLI, native ACP |
| **OpenCode** | ACP | sst/opencode, native ACP |
| **Hermes** | ACP | Nous Research coding agent, native ACP, OAuth login |
| **Kiro** | ACP | AWS Kiro CLI, native ACP, multi-agent configs |
| **OpenAI Codex** | Custom JSON-RPC | Via official codex-app-server-sdk |
| Any ACP agent | ACP | Generic adapter — bring your own |

## Quick Start

```bash
cargo install agentline
agentline                                  # auto-creates ~/.agentline/config.toml
$EDITOR ~/.agentline/config.toml           # set IM credentials + choose agent
agentline login                            # WeChat QR scan (if using WeChat)
agentline                                  # start bridging
```

That's it. Messages sent to your bot are routed to the coding agent; responses stream back in real-time.

## Architecture

```
crates/
├── bridge/          Core runtime: traits, message routing, permission engine, i18n
├── cli/             Binary entry point, config, service management, web dashboard
├── im/
│   ├── wechat/      iLink Bot API adapter
│   ├── dingtalk/    DingTalk Stream adapter
│   ├── feishu/      Feishu webhook + event adapter
│   └── telegram/    Telegram Bot API long-polling adapter
└── agent/
    ├── acp/         Generic ACP transport (shared by 7 agents)
    ├── claude-code/ Claude Code = ACP + env/settings plumbing
    ├── kimi/        Kimi = ACP + CLI launch config
    ├── qoder/       Qoder = ACP + personal access token support
    ├── opencode/    OpenCode = ACP + CLI launch config
    ├── hermes/      Hermes = ACP + OAuth login
    ├── kiro/        Kiro = ACP + multi-agent routing
    ├── gemini/      Gemini = ACP + CLI launch config
    └── codex/       Codex = custom JSON-RPC via official SDK
```

**Design principle**: ACP is a protocol, not a product. The transport layer (`agentline-agent-acp`) handles all protocol mechanics. Adding a new ACP agent is ~70 lines of config wrapper — no protocol code required.

## Key Features

### Intelligent Message Routing
- Paragraph-aware throttling (buffer to boundary / 600 chars / 2s) — no chat spam
- Per-session state isolation with configurable idle timeout
- Multi-session support with `/sessions` management

### Permission & Security
- Interactive permission delegation: agent asks → user confirms in chat (`y` / `n` / `s` for session-grant)
- `/yolo` mode for trusted sessions, auto-resets on new session
- User whitelist per IM platform

### Reliability
- **Crash auto-recovery**: agent process dies → automatic respawn (up to 5 retries, 2s cooldown)
- **Orphan process cleanup**: full process tree kill on shutdown via session-level reaping (macOS `setsid` + `proc_listpids`)
- **PID file tracking**: stale processes from previous crashes are cleaned on startup
- **Single-instance lock**: prevents duplicate daemons racing on the same IM token

### Operations
- Background service mode via launchd (auto-start, auto-restart)
- Built-in web dashboard (status, logs, WeChat QR login)
- macOS menu bar app for quick daemon control
- Structured logging with configurable levels
- Proxy injection for LAN/corporate environments (RFC-1918 auto-bypass)

### Internationalization
- Full i18n support (Chinese / English), runtime-switchable
- All user-facing strings externalized to YAML locale files
- Web dashboard fully localized via `vue-i18n`, syncs with `bridge.locale` config

## Configuration

```toml
# Each IM has its own enable flag — toggle independently
[im.feishu]
enable = true
app_id = "cli_xxxxx"
app_secret = "xxxxx"
verification_token = "xxxxx"
webhook_bind = "0.0.0.0:9000"
allowed_users = []                    # empty = allow all

[im.telegram]
enable = false
bot_token = ""
allowed_users = []

[agent]
backend = "claude-code"               # 8 built-in options + generic "acp"

[bridge]
default_cwd = ""                      # empty = auto-isolate per agent
locale = "en"                         # "zh-CN" | "en"
session_idle_timeout_secs = 7200      # auto-reset after 2h idle

[web]
enable = true
bind = "127.0.0.1:7681"

[proxy]
http = ""                             # injected into all agent subprocesses
```

See [`config.example.toml`](config.example.toml) for the full reference with all options documented.

## Deployment

### Foreground (development)

```bash
agentline
```

### Background Service (production)

```bash
agentline service install      # launchd plist, auto-start on boot
agentline service status       # check PID & health
agentline service logs --tail  # stream logs
```

### Menu Bar (macOS)

```bash
cargo install --path crates/tray
agentline-tray install
```

## Building from Source

```bash
git clone https://github.com/seven-tt/agentline
cd agentline
cargo build --release
./target/release/agentline --help
```

Requirements: Rust 1.85+, macOS / Linux.

## How It Works

The bridge is a `tokio::select!`-driven event loop that multiplexes between the IM inbound stream and agent update stream:

1. **Message arrives** from IM → parsed into `InboundMessage` → routed to session
2. **Session management** → `ensure_session` lazily creates agent sessions with proper cwd
3. **Prompt dispatch** → message forwarded to agent via `AgentBackend::prompt()`
4. **Streaming response** → agent updates (`AgentUpdate`) streamed back, throttled, and formatted per IM platform
5. **Permission flow** → agent requests permission → bridge renders interactive prompt → user replies → bridge resolves

Process lifecycle is managed at the OS level: `setsid` creates isolated sessions, `kill_session` reaps entire trees (even after `setpgid` escapes), and `kill_on_drop` provides belt-and-suspenders safety.

## Acknowledgements

- [agent-client-protocol](https://github.com/agentclientprotocol/rust-sdk) — the ACP Rust SDK powering multi-agent interop
- [claude-code-acp](https://github.com/zed-industries/claude-code-acp) — Claude Code's ACP bridge by Zed Industries
- [acp-cli](https://github.com/motosan-dev/acp-cli) — architecture inspiration for ACP client design
- [openclaw-weixin](https://github.com/hao-ji-xing/openclaw-weixin) — iLink Bot API protocol reference

## License

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE).

---

<p align="center">
  <sub>Built with Rust. Designed for teams who ship.</sub>
</p>
