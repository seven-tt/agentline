import type {
  OverviewData,
  ChannelsConfig,
  TransportConfig,
  AgentsData,
  AgentConfigIn,
  InstallResult,
  CheckUpdateResult,
  ProjectItem,
  SettingsConfig,
  LoginStatus,
} from './types'

const BASE = ''

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`)
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`)
  return res.json()
}

async function post<T = string>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  })
  if (!res.ok) throw new Error(`POST ${path}: ${res.status}`)
  const ct = res.headers.get('content-type') ?? ''
  if (ct.includes('json')) return res.json()
  return (await res.text()) as unknown as T
}

async function put<T = string>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(`PUT ${path}: ${res.status}`)
  const ct = res.headers.get('content-type') ?? ''
  if (ct.includes('json')) return res.json()
  return (await res.text()) as unknown as T
}

async function getText(path: string): Promise<string> {
  const res = await fetch(`${BASE}${path}`)
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`)
  return res.text()
}

export const api = {
  // Overview
  getOverview: () => get<OverviewData>('/api/overview'),

  // Channels (IM)
  getChannels: () => get<ChannelsConfig>('/api/channels'),
  saveChannels: (data: Partial<ChannelsConfig>) => post('/api/channels', data),
  wechatLoginStart: () => post('/api/channels/wechat/login/start'),
  wechatLoginCancel: () => post('/api/channels/wechat/login/cancel'),
  wechatLoginStatus: () => get<LoginStatus>('/api/channels/wechat/login/status'),
  wechatLoginQrUrl: `${BASE}/api/channels/wechat/login/qr.png`,

  // Transport
  getTransport: () => get<TransportConfig>('/api/transport'),
  saveTransport: (data: Partial<TransportConfig>) => post('/api/transport', data),

  // Agent
  getAgents: () => get<AgentsData>('/api/agents'),
  saveAgentConfig: (data: AgentConfigIn) => post('/api/agents/config', data),
  installAgent: (id: string) => post<InstallResult>(`/api/agents/${id}/install`),
  checkAgentUpdate: (id: string) => post<CheckUpdateResult>(`/api/agents/${id}/check-update`),

  // Projects
  getProjects: () => get<ProjectItem[]>('/api/projects'),
  saveProjects: (data: ProjectItem[]) => put('/api/projects', data),

  // Settings
  getSettings: () => get<SettingsConfig>('/api/settings'),
  saveSettings: (data: Partial<SettingsConfig>) => post('/api/settings', data),
  restart: () => post('/api/settings/restart'),

  // Logs
  getLogs: () => getText('/api/logs'),

  // System update
  checkSystemUpdate: () => get<{ has_update: boolean; current: string; latest: string }>('/api/system/check-update'),
  triggerSystemUpdate: () => post('/api/system/update'),
}
