// ─── Overview ──────────────────────────────────────────────────

export interface SessionInfo {
  id: string
  user: string
  active: boolean
  cwd: string
}

export interface ImStatus {
  id: string
  enabled: boolean
  healthy: boolean
  sessions: SessionInfo[]
}

export interface OverviewData {
  version: string
  uptime_secs: number
  pid: number
  agent_backend: string
  ims: ImStatus[]
}

// ─── Agent ─────────────────────────────────────────────────────

export interface AgentItem {
  id: string
  installed: boolean
  version: string | null
  status: 'ready' | 'needs_login' | 'not_installed'
}

export interface CodexConfig {
  model: string
  sandbox_mode: string
  approval_mode: string
}

export interface QoderConfig {
  personal_access_token: string
}

export interface OpencodeConfig {
  base_url: string
  api_key: string
}

export interface KimiConfig {
  access_token: string
}

export interface AgentsData {
  backend: string
  platform: string
  list: AgentItem[]
  configs: {
    codex: CodexConfig
    qoder: QoderConfig
    opencode: OpencodeConfig
    kimi: KimiConfig
  }
}

export interface AgentConfigIn {
  backend?: string
  codex?: Partial<CodexConfig>
  qoder?: Partial<QoderConfig>
  opencode?: Partial<OpencodeConfig>
  kimi?: Partial<KimiConfig>
}

export interface InstallResult {
  success: boolean
  error?: string
}

export interface CheckUpdateResult {
  current: string | null
  latest: string | null
  has_update: boolean
}

// ─── Channels (IM) ─────────────────────────────────────────────

export interface WechatConfig {
  enable: boolean
  allowed_users: string[]
  typing_interval_ms: number
  logged_in: boolean
}

export interface DingtalkConfig {
  enable: boolean
  client_id: string
  client_secret: string
  allowed_users: string[]
}

export interface FeishuConfig {
  enable: boolean
  app_id: string
  app_secret: string
  verification_token: string
  encrypt_key: string
  webhook_bind: string
  allowed_users: string[]
}

export interface TelegramConfig {
  enable: boolean
  bot_token: string
  api_base: string
  allowed_users: string[]
}

export interface ChannelsConfig {
  wechat: WechatConfig
  dingtalk: DingtalkConfig
  feishu: FeishuConfig
  telegram: TelegramConfig
}

export interface LoginStatus {
  state: 'idle' | 'starting' | 'waiting_scan' | 'completed' | 'failed'
  message: string
}

// ─── Settings ──────────────────────────────────────────────────

export interface SettingsConfig {
  bridge: {
    default_cwd: string
    session_idle_timeout_secs: number
    locale: string
  }
  web: { bind: string }
  proxy: { http: string; https: string; no_proxy: string }
  log: { level: string }
}

// ─── Projects ──────────────────────────────────────────────────

export interface ProjectItem {
  name: string
  git_url: string
}

// ─── UI ────────────────────────────────────────────────────────

export type ViewName = 'overview' | 'channels' | 'agent' | 'projects' | 'settings' | 'logs'

export interface ToastItem {
  id: number
  type: 'success' | 'error' | 'info'
  message: string
}
