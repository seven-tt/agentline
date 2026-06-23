<p align="center">
  <h1 align="center">Agentline</h1>
  <p align="center">
    <strong>一个二进制，连接任意 IM 与任意编码 Agent</strong>
  </p>
  <p align="center">
    <a href="https://github.com/seven-tt/agentline/actions/workflows/ci.yml"><img src="https://github.com/seven-tt/agentline/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License"></a>
    <a href="#"><img src="https://img.shields.io/badge/rust-1.89+-orange" alt="Rust"></a>
  </p>
</p>

[English](README.md)

---

Agentline 是一个高性能 Rust 桥接器，把任意即时通讯平台变成功能完整的编码 agent 交互界面。部署一次，团队就能在日常使用的聊天工具中直接获得 AI 编程能力——无需安装新应用，零上下文切换。

```
 IM Channels            Core             Agent Backends
+------------+     +-----------+     +------------------+
|  微信      |     |           |     |  Claude Code     |
|  钉钉      +---->+  Bridge   +---->+  Gemini CLI      |
|  飞书      |     |  (Actor)  |     |  Kimi / Qoder    |
|  Telegram  |     |     |     |     |  Hermes / Kiro   |
+------------+     +--+--+--+-+     +------------------+
 4 个 IM 适配器       |     |         9 个后端 + 自定义
 WebSocket / 轮询     |     |         ACP 协议
                      v     v
               +------+  +-------+
               | MCP  |  | Iroh  |
               |Server|  |  P2P  |
               +------+  +-------+
```

## 为什么选择 Agentline？

- **零摩擦接入** — 团队每天都在用微信/钉钉/飞书/Telegram，加个 bot 就行
- **Agent 无关** — Claude Code、Gemini、Kimi、Codex……改一行配置即切换，不锁定任何厂商
- **Actor 运行时** — 基于 Tokio channel 的无锁消息路由，Bridge Actor 独占所有可变状态
- **生产级可靠性** — 崩溃自动恢复、孤儿进程清理、会话隔离、进程树全生命周期管理
- **私有化部署** — 完全运行在你自己的基础设施上，数据不出网
- **单二进制** — 一条 `cargo install`，无运行时依赖

## 支持平台

### IM 适配器

| 平台 | 协议 | 状态 | 媒体支持 |
|------|------|------|----------|
| **微信** | iLink 个人 Bot API | 稳定 | 文字、图片、语音、文件 |
| **钉钉** | Stream API（WebSocket） | 稳定 | 文字、图片、语音、视频、文件 |
| **飞书** | WebSocket 长连接 + REST API | 稳定 | 文字、图片 |
| **Telegram** | Bot API（长轮询） | 稳定 | 文字、图片、语音、音频、视频、文件 |

### Agent 后端

| Agent | 协议 | 说明 |
|-------|------|------|
| **Claude Code** | ACP | 权限委托、elicitation 交互 |
| **Gemini CLI** | ACP | Google gemini-cli |
| **Kimi Code** | ACP | Moonshot kimi-cli |
| **Qoder** | ACP | Personal access token 支持 |
| **OpenCode** | ACP | sst/opencode |
| **Hermes** | ACP | Nous Research，OAuth 登录 |
| **Kiro** | ACP | AWS Kiro CLI，多 agent 配置 |
| **OpenAI Codex** | JSON-RPC | 通过官方 codex-app-server-sdk |
| **任意 ACP agent** | ACP | 通用适配器——自带你的 agent |

## 快速开始

支持 **macOS**、**Linux** 和 **Windows**。

### 一键安装（推荐）

```bash
# macOS / Linux — 安装托盘应用 + CLI
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash

# Windows (PowerShell) — 下载并运行安装程序
irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex

# 仅安装无头版（无 GUI 的服务器）
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash -s -- --headless
```

macOS 会挂载 `.dmg` 并将 `AgentlineTray.app` 复制到 `/Applications/`。Linux 下载 `.deb` 包通过 `dpkg` 安装。Windows 下载并运行 Inno Setup 安装程序。Linux 会自动检测无图形界面环境并降级为仅安装 CLI。

### 配置并运行

```bash
agentline                            # 自动创建 ~/.agentline/config.toml
$EDITOR ~/.agentline/config.toml     # 配置 IM 凭据 + 选择 agent
agentline                            # 开始桥接
```

微信后端需先运行 `agentline login` 扫码登录。

## 架构

```
crates/
├── bridge/          核心运行时：actor、消息路由、权限引擎、i18n
├── cli/             二进制入口、配置管理、服务管理、Web Dashboard、MCP Server
├── permission/      分层权限引擎（Shell 命令 + MCP 工具）
├── tray/            跨平台系统托盘应用（含应用内自动更新）
├── telegramify/     Markdown → Telegram MarkdownV2 转换器
├── im/
│   ├── core/        共享 IM trait（ImAdapter、InboundHandler）
│   ├── wechat/      iLink Bot API 适配器
│   ├── dingtalk/    钉钉 Stream（WebSocket）适配器 + 媒体下载
│   ├── feishu/      飞书 WebSocket 长连接适配器 + 媒体下载
│   └── telegram/    Telegram Bot API（长轮询）适配器 + 媒体下载
├── agent/
│   ├── acp/         通用 ACP 传输层（8 个后端共用）
│   ├── claude-code/ Claude Code = ACP + 环境/设置注入
│   ├── kimi/        Kimi = ACP + CLI 启动配置
│   ├── qoder/       Qoder = ACP + personal access token
│   ├── opencode/    OpenCode = ACP + CLI 启动配置
│   ├── hermes/      Hermes = ACP + OAuth 登录
│   ├── kiro/        Kiro = ACP + 多 agent 路由
│   ├── gemini/      Gemini = ACP + CLI 启动配置
│   └── codex/       Codex = 通过官方 SDK 的 JSON-RPC 协议
└── transport/
    ├── core/        多连接传输层（TCP/Unix Socket）
    └── iroh/        Iroh P2P 传输，NAT 穿透远程访问
```

### Bridge 运行时

Bridge 采用 **Actor 模型**：轻量的 `Bridge` handle（可 Clone，仅持有 channel sender）将命令转发给唯一的 `BridgeActor`，后者独占所有可变状态——零 `Mutex`，零 `lock().await`。

```
  IM 适配器 ──┐
              ├──▶ Bridge (handle)  ──mpsc──▶  BridgeActor (独占 state)
  CLI / 测试 ──┘   Clone, Send                 tokio::select! 事件循环
```

Actor 在 `tokio::select!` 循环中多路复用 IM 入站消息和内部命令：

1. **消息到达** IM → 解析 → 路由到 session
2. **Session 管理** → 惰性创建 agent session，自动设置工作目录
3. **Prompt 分发** → 通过 `AgentBackend::prompt()` 转发给 agent
4. **流式响应** → 按 IM 平台节流、格式化后流式回传
5. **权限流程** → agent 请求 → 聊天中交互式确认 → 用户回复 → 解析应答

### 设计理念

- **ACP 是协议，不是产品。** 传输层（`agentline-agent-acp`）处理所有协议细节。新增 ACP agent 只需 ~70 行配置包装。
- **IM 适配器完全自包含。** 每个适配器实现 `ImAdapter` + `InboundHandler`，不感知其他适配器或 agent 后端。
- **进程生命周期由 OS 管理。** Unix 上 `setsid` 创建隔离 session，`kill_session` 收割整个进程树；Windows 上 `sysinfo` 遍历进程树。`kill_on_drop` 作为最后安全网。

## 核心特性

### 消息路由
- 段落感知节流（缓冲到段落边界 / 600 字符 / 2 秒）
- 按 session 状态隔离，可配置空闲超时
- 多会话支持，`/sessions` 统一管理

### 权限与安全
- 交互式权限委托：agent 申请 → 用户在聊天中确认（`y` / `n` / `s` session 级授权）
- MCP 工具分层权限：agentline 自有工具自动放行，第三方工具需确认
- `/yolo` 模式用于可信 session，新 session 自动复位
- 按 IM 平台的用户白名单

### 可靠性
- **崩溃恢复**：agent 进程死亡 → 自动重启（最多 5 次，间隔 2s）
- **孤儿清理**：shutdown 时杀掉整个进程树
- **PID 追踪**：上次崩溃残留的进程在启动时自动清理
- **单实例锁**：防止重复 daemon

### 运维
- 后台服务（macOS launchd 托管，开机自启自动拉起）
- 内嵌 Web Dashboard（状态、日志、微信扫码登录、Agent 配置、P2P 传输）
- 内嵌 MCP Server（`POST /mcp`），向 Agent 暴露项目工具
- 跨平台系统托盘 app，支持应用内自动更新
- Iroh P2P 传输，无需端口转发即可远程访问
- 结构化日志，可配置级别
- 代理注入，RFC-1918 地址自动 bypass

### 国际化
- 中文 / 英文，运行时切换
- 所有用户可见字符串外置到 YAML locale 文件
- Web Dashboard 通过 `vue-i18n` 本地化

## 配置

```toml
[im.feishu]
enable = true
app_id = "cli_xxxxx"
app_secret = "xxxxx"
allowed_users = []                    # 空 = 放过所有

[im.telegram]
enable = false
bot_token = ""
allowed_users = []

[agent]
backend = "claude-code"               # 9 个内置选项 + 通用 "acp"

[bridge]
default_cwd = ""                      # 空 = 按 agent 自动隔离
locale = "zh-CN"                      # "zh-CN" | "en"
session_idle_timeout_secs = 7200      # 空闲 2 小时后自动重置

[web]
enable = true
bind = "127.0.0.1:7681"

[proxy]
http = ""                             # 注入到所有 agent 子进程
```

完整配置参考见 [`config.example.toml`](config.example.toml)。

## 部署

```bash
# 前台运行
agentline

# 后台服务（macOS）
agentline service install      # launchd plist，开机自启
agentline service status
agentline service logs --tail

# 系统托盘
agentline-tray
```

## 从源码构建

```bash
git clone https://github.com/seven-tt/agentline
cd agentline
cargo build --release --bin agentline        # CLI
cargo build --release --bin agentline-tray   # 系统托盘
```

## 致谢

- [agent-client-protocol](https://github.com/agentclientprotocol/rust-sdk) — ACP Rust SDK
- [claude-code-acp](https://github.com/zed-industries/claude-code-acp) — Zed Industries 的 Claude Code ACP 桥接
- [acp-cli](https://github.com/motosan-dev/acp-cli) — 架构设计参考
- [openclaw-weixin](https://github.com/hao-ji-xing/openclaw-weixin) — iLink Bot API 协议参考

## License

基于 [Apache License, Version 2.0](LICENSE-APACHE) 授权。

---

<p align="center">
  <sub>用 Rust 构建，为高效团队而生。</sub>
</p>
