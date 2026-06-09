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
|  WeChat    |     |           |     |  Claude Code     |
|  DingTalk  +---->+  Bridge   +---->+  Gemini CLI      |
|  Feishu    |     |           |     |  Kimi / Qoder    |
|  Telegram  |     |           |     |  Hermes / Kiro   |
+------------+     +-----------+     +------------------+
 each has own                         9 backends + custom
 enable flag                          ACP adapter
```

## 为什么选择 Agentline？

- **零摩擦接入** — 团队每天都在用微信/钉钉/飞书，加个 bot 就行，无需学习新工具
- **Agent 无关** — Claude Code、Gemini、Kimi、Codex……改一行配置即切换，不锁定任何厂商
- **生产级可靠性** — 崩溃自动恢复、孤儿进程清理、会话隔离、进程树全生命周期管理
- **私有化部署** — 完全运行在你自己的基础设施上，数据不出网
- **单二进制，极简部署** — 一条 `cargo install`，无运行时依赖，~10MB 体积

## 支持平台

### IM 适配器

| 平台 | 协议 | 状态 |
|------|------|------|
| **微信** | iLink 个人 Bot API | 稳定 |
| **钉钉** | Stream API（原生 WebSocket） | 稳定 |
| **飞书** | 事件订阅 + Bot API | 稳定 |
| **Telegram** | Bot API（长轮询） | 稳定 |

### Agent 后端

| Agent | 协议 | 说明 |
|-------|------|------|
| **Claude Code** | ACP | 完整支持：权限委托、elicitation 交互 |
| **Gemini CLI** | ACP | Google gemini-cli，原生 ACP |
| **Kimi Code** | ACP | Moonshot kimi-cli，原生 ACP |
| **Qoder** | ACP | Qoder CLI，原生 ACP |
| **OpenCode** | ACP | sst/opencode，原生 ACP |
| **Hermes** | ACP | Nous Research 编码代理，原生 ACP，OAuth 登录 |
| **Kiro** | ACP | AWS Kiro CLI，原生 ACP，支持多 agent 配置 |
| **OpenAI Codex** | 自有 JSON-RPC | 通过官方 codex-app-server-sdk |
| 任意 ACP agent | ACP | 通用适配器——自带你的 agent |

## 快速开始

需要 **Rust 1.89+**，支持 **macOS**、**Linux** 和 **Windows**。

### 无界面版（CLI）

```bash
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash
agentline                                    # 自动创建 ~/.agentline/config.toml
$EDITOR ~/.agentline/config.toml             # 配置 IM 凭据 + 选择 agent
agentline login                              # 微信扫码登录（仅微信后端需要）
agentline                                    # 开始桥接
```

### 有界面版（系统托盘）

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash -s -- --tray

# Windows (PowerShell)
irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex
```

就这么简单。发给 bot 的消息会路由到编码 agent，响应实时流式回传。

## 架构

```
crates/
├── bridge/          核心运行时：trait 定义、消息路由、权限引擎、i18n
├── cli/             二进制入口、配置管理、服务管理、Web Dashboard
├── im/
│   ├── wechat/      iLink Bot API 适配器
│   ├── dingtalk/    钉钉 Stream 适配器
│   ├── feishu/      飞书 webhook + 事件适配器
│   └── telegram/    Telegram Bot API 长轮询适配器
└── agent/
    ├── acp/         通用 ACP 传输层（7 个 agent 共用）
    ├── claude-code/ Claude Code = ACP + 环境/设置注入
    ├── kimi/        Kimi = ACP + CLI 启动配置
    ├── qoder/       Qoder = ACP + personal access token
    ├── opencode/    OpenCode = ACP + CLI 启动配置
    ├── hermes/      Hermes = ACP + OAuth 登录
    ├── kiro/        Kiro = ACP + 多 agent 路由
    ├── gemini/      Gemini = ACP + CLI 启动配置
    └── codex/       Codex = 通过官方 SDK 的自有 JSON-RPC 协议
```

**设计理念**：ACP 是协议，不是产品。传输层（`agentline-agent-acp`）处理所有协议细节。新增一个 ACP agent 只需 ~70 行配置包装——不碰协议代码。

## 核心特性

### 智能消息路由
- 段落感知节流（缓冲到段落边界 / 600 字符 / 2 秒）——不刷屏
- 按 session 状态隔离，可配置空闲超时
- 多会话支持，`/sessions` 统一管理

### 权限与安全
- 交互式权限委托：agent 申请 → 用户在聊天中确认（`y` / `n` / `s` session 级授权）
- `/yolo` 模式用于可信 session，新 session 自动复位
- 按 IM 平台的用户白名单

### 可靠性
- **崩溃自动恢复**：agent 进程死亡 → 自动重启（最多 5 次，间隔 2s）
- **孤儿进程清理**：shutdown 时通过 session 级 reaping 杀掉整个进程树（macOS `setsid` + `proc_listpids`，Windows `sysinfo` 进程树遍历）
- **PID 文件追踪**：上次崩溃残留的进程在启动时自动清理
- **单实例锁**：防止重复 daemon 抢同一个 IM token

### 运维
- 后台服务模式（macOS launchd 托管，开机自启，崩溃自动拉起；Windows 可用任务计划程序或直接运行）
- 内嵌 Web Dashboard（状态、日志、微信扫码登录）
- 跨平台系统托盘 app（macOS / Windows / Linux）快速控制 daemon
- 结构化日志，可配置级别
- 代理注入，自动绕过局域网（RFC-1918 地址自动 bypass）

### 国际化
- 完整 i18n 支持（中文 / 英文），运行时可切换
- 所有用户可见字符串外置到 YAML locale 文件
- Web Dashboard 通过 `vue-i18n` 完整本地化，与 `bridge.locale` 配置联动

## 配置

```toml
# 每个 IM 独立启停
[im.feishu]
enable = true
app_id = "cli_xxxxx"
app_secret = "xxxxx"
verification_token = "xxxxx"
webhook_bind = "0.0.0.0:9000"
allowed_users = []                    # 空 = 放过所有

[im.telegram]
enable = false
bot_token = ""
allowed_users = []

[agent]
backend = "claude-code"               # 8 个内置选项 + 通用 "acp"

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

### 前台运行（开发）

```bash
agentline
```

### 后台服务（生产，macOS）

```bash
agentline service install      # launchd plist，开机自启
agentline service status       # 查看 PID 和健康状态
agentline service logs --tail  # 实时日志流
```

> Windows 上直接运行 `agentline run` 或使用任务计划程序。

### 系统托盘（macOS / Windows / Linux）

```bash
agentline-tray
```

## 从源码构建

```bash
git clone https://github.com/seven-tt/agentline
cd agentline
cargo build --release --bin agentline        # CLI
cargo build --release --bin agentline-tray   # 托盘程序（macOS / Windows / Linux）
```

然后将 `target/release/` 中的二进制文件复制到 `$PATH` 即可。

## 工作原理

Bridge 是一个 `tokio::select!` 驱动的事件循环，在 IM 入站流和 agent 更新流之间多路复用：

1. **消息到达** IM → 解析为 `InboundMessage` → 路由到 session
2. **Session 管理** → `ensure_session` 惰性创建 agent session 并设置工作目录
3. **Prompt 分发** → 消息通过 `AgentBackend::prompt()` 转发给 agent
4. **流式响应** → agent 更新（`AgentUpdate`）流式回传，经节流和格式化后发送到 IM
5. **权限流程** → agent 请求权限 → bridge 渲染交互式提示 → 用户回复 → bridge 解析并应答

进程生命周期在 OS 层面管理：Unix 上 `setsid` 创建隔离 session，`kill_session` 收割整个进程树（即使子进程通过 `setpgid` 逃逸也能追回）；Windows 上通过 `sysinfo` 遍历进程树实现等效清理。`kill_on_drop` 在所有平台上作为最后安全网。

## 致谢

- [agent-client-protocol](https://github.com/agentclientprotocol/rust-sdk) — 驱动多 agent 互操作的 ACP Rust SDK
- [claude-code-acp](https://github.com/zed-industries/claude-code-acp) — Zed Industries 的 Claude Code ACP 桥接
- [acp-cli](https://github.com/motosan-dev/acp-cli) — ACP 客户端架构设计参考
- [openclaw-weixin](https://github.com/hao-ji-xing/openclaw-weixin) — iLink Bot API 协议参考

## License

基于 [Apache License, Version 2.0](LICENSE-APACHE) 授权。

---

<p align="center">
  <sub>用 Rust 构建，为高效团队而生。</sub>
</p>
