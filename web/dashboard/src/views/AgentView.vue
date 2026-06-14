<script setup lang="ts">
import { inject, ref, reactive, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import type { AgentsData, AgentItem } from '../types'
import { api } from '../api'

const { t } = useI18n()
const addToast = inject<(type: 'success' | 'error' | 'info', msg: string) => void>('addToast')!

interface AgentMeta {
  id: string
  name: string
  abbr: string
  desc: string
  unixOnly?: boolean
}

const AGENT_META: AgentMeta[] = [
  { id: 'claude-code', name: 'Claude Code', abbr: 'CC', desc: 'agent.desc_claude_code' },
  { id: 'kimi', name: 'Kimi Code', abbr: 'Ki', desc: 'agent.desc_kimi' },
  { id: 'qoder', name: 'Qoder', abbr: 'Qo', desc: 'agent.desc_qoder', unixOnly: true },
  { id: 'opencode', name: 'OpenCode', abbr: 'OC', desc: 'agent.desc_opencode' },
  { id: 'kiro', name: 'Kiro', abbr: 'Kr', desc: 'agent.desc_kiro' },
  { id: 'gemini', name: 'Gemini CLI', abbr: 'Gm', desc: 'agent.desc_gemini' },
  { id: 'hermes', name: 'Hermes Agent', abbr: 'Hm', desc: 'agent.desc_hermes' },
  { id: 'codex', name: 'Codex CLI', abbr: 'Cx', desc: 'agent.desc_codex' },
]

function agentStep(agent: AgentItem | undefined): number {
  if (!agent || agent.status === 'not_installed') return 1
  if (agent.status === 'needs_login') return 2
  return 3
}

const data = reactive<AgentsData>({
  backend: '',
  platform: '',
  list: [],
  configs: {
    codex: { model: '', sandbox_mode: 'workspace-write', approval_mode: 'never', api_key: '' },
    qoder: { personal_access_token: '' },
    opencode: { base_url: '', api_key: '' },
    kimi: { access_token: '' },
    gemini: { api_key: '' },
  },
})

const selectedAgent = ref<string | null>(null)
const installing = ref<string | null>(null)
const loginMethod = reactive<Record<string, string>>({
  qoder: 'token',
  kimi: 'cli',
  opencode: 'token',
  codex: 'cli',
  gemini: 'cli',
})

const STATUS_ORDER: Record<string, number> = { ready: 0, needs_login: 1, not_installed: 2 }
const isWindows = computed(() => data.platform === 'windows')
const sortedAgents = computed(() =>
  AGENT_META
    .filter((m) => !(m.unixOnly && isWindows.value))
    .sort((a, b) => {
      const sa = STATUS_ORDER[getAgent(a.id)?.status ?? 'not_installed'] ?? 2
      const sb = STATUS_ORDER[getAgent(b.id)?.status ?? 'not_installed'] ?? 2
      return sa - sb
    }),
)

onMounted(async () => {
  try {
    const result = await api.getAgents()
    Object.assign(data, result)
  } catch { /* ignore */ }
})

function getAgent(id: string): AgentItem | undefined {
  return data.list.find((a) => a.id === id)
}

function statusBadge(agent: AgentItem | undefined) {
  if (!agent) return { class: 'badge-muted', text: '...' }
  switch (agent.status) {
    case 'ready':
      return { class: 'badge-green', text: t('agent.status_ready') }
    case 'needs_login':
      return { class: 'badge-amber', text: t('agent.status_needs_login') }
    case 'not_installed':
      return { class: 'badge-muted', text: t('agent.status_not_installed') }
    default:
      return { class: 'badge-muted', text: agent.status }
  }
}

function selectAgent(id: string) {
  selectedAgent.value = id
}

function closeModal() {
  selectedAgent.value = null
}

async function setActive(id: string) {
  try {
    await api.saveAgentConfig({ backend: id })
    data.backend = id
    addToast('success', t('agent.switched_to', { id }))
    closeModal()
  } catch (e: any) {
    addToast('error', e.message)
  }
}

async function refreshAgents() {
  try {
    const result = await api.getAgents()
    Object.assign(data, result)
    addToast('info', t('common.refreshed'))
  } catch { /* ignore */ }
}

async function installAgent(id: string) {
  installing.value = id
  try {
    const result = await api.installAgent(id)
    if (result.success) {
      addToast('success', t('agent.install_success', { id }))
      const refreshed = await api.getAgents()
      Object.assign(data, refreshed)
    } else {
      addToast('error', t('agent.install_failed', { error: result.error }))
    }
  } catch (e: any) {
    addToast('error', e.message)
  } finally {
    installing.value = null
  }
}
</script>

<template>
  <div class="agent-grid">
    <div
      v-for="meta in sortedAgents"
      :key="meta.id"
      :class="['agent-card', {
        'is-active': data.backend === meta.id,
        'is-not-installed': getAgent(meta.id)?.status === 'not_installed'
      }]"
      @click="selectAgent(meta.id)"
    >
      <div class="agent-card-head">
        <div :class="['agent-icon', meta.id]">{{ meta.abbr }}</div>
        <span class="agent-card-name">{{ meta.name }}</span>
        <span
          v-if="data.backend === meta.id"
          class="agent-card-tag"
        >{{ $t('agent.tag_active') }}</span>
        <span
          v-else-if="getAgent(meta.id)?.status === 'not_installed'"
          class="agent-card-tag tag-gray"
        >{{ $t('agent.status_not_installed') }}</span>
      </div>
      <div class="agent-card-desc">{{ $t(meta.desc) }}</div>
      <div class="agent-card-footer">
        <span
          class="badge"
          :class="statusBadge(getAgent(meta.id)).class"
        >
          <span v-if="getAgent(meta.id)?.status === 'ready'" class="badge-dot"></span>
          <span v-if="getAgent(meta.id)?.status === 'needs_login'" class="badge-dot"></span>
          {{ statusBadge(getAgent(meta.id)).text }}
        </span>
        <span class="agent-card-link">{{ $t('agent.click_manage') }}</span>
      </div>
    </div>
  </div>

  <Teleport to="body">
    <div v-if="selectedAgent" class="modal-overlay" @click.self="closeModal">
      <div class="modal">
        <div class="modal-header">
          <div :class="['agent-icon', selectedAgent]">
            {{ AGENT_META.find((a) => a.id === selectedAgent)?.abbr }}
          </div>
          <div class="modal-header-info">
            <div class="modal-title">
              {{ AGENT_META.find((a) => a.id === selectedAgent)?.name }}
            </div>
            <div class="modal-desc">
              {{ $t(AGENT_META.find((a) => a.id === selectedAgent)?.desc ?? '') }}
            </div>
          </div>
          <button class="modal-close" @click="closeModal">&times;</button>
        </div>
        <div class="modal-body">
          <!-- Stepper -->
          <div class="stepper">
            <div :class="['step', { active: agentStep(getAgent(selectedAgent!)) >= 1, current: agentStep(getAgent(selectedAgent!)) === 1 }]">
              <span class="step-num">{{ agentStep(getAgent(selectedAgent!)) > 1 ? '✓' : '1' }}</span><span class="step-label">{{ $t('agent.step_install') }}</span>
            </div>
            <div class="step-line" :class="{ done: agentStep(getAgent(selectedAgent!)) >= 2 }"></div>
            <div :class="['step', { active: agentStep(getAgent(selectedAgent!)) >= 2, current: agentStep(getAgent(selectedAgent!)) === 2 }]">
              <span class="step-num">{{ agentStep(getAgent(selectedAgent!)) > 2 ? '✓' : '2' }}</span><span class="step-label">{{ $t('agent.step_login') }}</span>
            </div>
            <div class="step-line" :class="{ done: agentStep(getAgent(selectedAgent!)) >= 3 }"></div>
            <div :class="['step', { active: agentStep(getAgent(selectedAgent!)) >= 3, current: agentStep(getAgent(selectedAgent!)) === 3 }]">
              <span class="step-num">{{ agentStep(getAgent(selectedAgent!)) >= 3 ? '✓' : '3' }}</span><span class="step-label">{{ $t('agent.step_ready') }}</span>
            </div>
          </div>

          <!-- Step 1: Not installed -->
          <div v-if="agentStep(getAgent(selectedAgent!)) === 1" class="step-content">
            <p class="text-muted">{{ $t('agent.install_hint') }}</p>
            <button
              class="btn btn-primary"
              :disabled="installing === selectedAgent"
              @click="installAgent(selectedAgent!)"
            >
              {{ installing === selectedAgent ? $t('agent.installing') : $t('agent.install_tpl', { name: AGENT_META.find(a => a.id === selectedAgent)?.name }) }}
            </button>
            <a class="link-muted" href="javascript:void(0)">{{ $t('agent.learn_more') }}</a>
          </div>

          <!-- Step 2: Needs login -->
          <div v-else-if="agentStep(getAgent(selectedAgent!)) === 2" class="step-content">
            <div class="login-header">
              <h4 class="step-title" style="margin: 0">{{ $t('agent.login_title') }}</h4>
              <select
                v-if="['qoder', 'kimi', 'opencode', 'codex', 'gemini'].includes(selectedAgent!)"
                class="login-method-select"
                v-model="loginMethod[selectedAgent!]"
              >
                <template v-if="selectedAgent === 'qoder'">
                  <option value="token">{{ $t('agent.pat_token') }}</option>
                  <option value="cli">{{ $t('agent.cli_login') }}</option>
                </template>
                <template v-if="selectedAgent === 'kimi'">
                  <option value="cli">{{ $t('agent.interactive_login') }}</option>
                  <option value="token">{{ $t('agent.access_token') }}</option>
                </template>
                <template v-if="selectedAgent === 'opencode'">
                  <option value="token">API Key</option>
                  <option value="cli">{{ $t('agent.cli_login') }}</option>
                </template>
                <template v-if="selectedAgent === 'codex'">
                  <option value="cli">{{ $t('agent.cli_login') }}</option>
                  <option value="token">API Key</option>
                </template>
                <template v-if="selectedAgent === 'gemini'">
                  <option value="cli">{{ $t('agent.interactive_login') }}</option>
                  <option value="token">API Key</option>
                </template>
              </select>
            </div>

            <!-- Qoder: PAT -->
            <template v-if="selectedAgent === 'qoder' && loginMethod.qoder === 'token'">
              <p class="text-muted">{{ $t('agent.qoder_pat_hint') }}</p>
              <div style="width: 100%">
                <div class="field">
                  <label class="field-label">personal_access_token</label>
                  <input type="password" v-model="data.configs.qoder.personal_access_token" class="input-mono" :placeholder="$t('agent.qoder_pat_placeholder')" />
                </div>
                <div class="flex-row-end mt-4">
                  <button class="btn btn-primary" @click="async () => {
                    try {
                      await api.saveAgentConfig({ qoder: data.configs.qoder })
                      addToast('success', t('agent.qoder_config_saved'))
                      const refreshed = await api.getAgents()
                      Object.assign(data, refreshed)
                    } catch (e: any) { addToast('error', e.message) }
                  }">{{ $t('agent.save_continue') }}</button>
                </div>
              </div>
            </template>

            <!-- Qoder: CLI login -->
            <template v-else-if="selectedAgent === 'qoder' && loginMethod.qoder === 'cli'">
              <p class="text-muted">{{ $t('agent.qoder_cli_hint') }}</p>
              <div class="cli-cmd">qodercli login</div>
              <p class="text-muted text-sm">{{ $t('agent.qoder_cli_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Kimi: CLI login -->
            <template v-else-if="selectedAgent === 'kimi' && loginMethod.kimi === 'cli'">
              <p class="text-muted">{{ $t('agent.kimi_cli_hint') }}</p>
              <div class="cli-cmd">kimi /login</div>
              <p class="text-muted text-sm">{{ $t('agent.kimi_cli_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Kimi: Access Token -->
            <template v-else-if="selectedAgent === 'kimi' && loginMethod.kimi === 'token'">
              <p class="text-muted">{{ $t('agent.kimi_token_hint') }}</p>
              <div style="width: 100%">
                <div class="field">
                  <label class="field-label">access_token</label>
                  <input type="password" v-model="data.configs.kimi.access_token" class="input-mono" :placeholder="$t('agent.kimi_token_placeholder')" />
                </div>
                <div class="flex-row-end mt-4">
                  <button class="btn btn-primary" @click="async () => {
                    try {
                      await api.saveAgentConfig({ kimi: data.configs.kimi })
                      addToast('success', t('agent.kimi_config_saved'))
                      const refreshed = await api.getAgents()
                      Object.assign(data, refreshed)
                    } catch (e: any) { addToast('error', e.message) }
                  }">{{ $t('agent.save_continue') }}</button>
                </div>
              </div>
            </template>

            <!-- OpenCode: API Key -->
            <template v-else-if="selectedAgent === 'opencode' && loginMethod.opencode === 'token'">
              <p class="text-muted">{{ $t('agent.opencode_token_hint') }}</p>
              <div style="width: 100%">
                <div class="field">
                  <label class="field-label">base_url</label>
                  <input type="text" v-model="data.configs.opencode.base_url" class="input-mono" :placeholder="$t('agent.opencode_url_placeholder')" />
                </div>
                <div class="field">
                  <label class="field-label">api_key</label>
                  <input type="password" v-model="data.configs.opencode.api_key" class="input-mono" :placeholder="$t('agent.opencode_key_placeholder')" />
                </div>
                <div class="flex-row-end mt-4">
                  <button class="btn btn-primary" @click="async () => {
                    try {
                      await api.saveAgentConfig({ opencode: data.configs.opencode })
                      addToast('success', t('agent.opencode_saved'))
                      const refreshed = await api.getAgents()
                      Object.assign(data, refreshed)
                    } catch (e: any) { addToast('error', e.message) }
                  }">{{ $t('agent.save_continue') }}</button>
                </div>
              </div>
            </template>

            <!-- OpenCode: CLI login -->
            <template v-else-if="selectedAgent === 'opencode' && loginMethod.opencode === 'cli'">
              <p class="text-muted">{{ $t('agent.opencode_cli_hint') }}</p>
              <div class="cli-cmd">opencode providers login</div>
              <p class="text-muted text-sm">{{ $t('agent.opencode_cli_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Codex: CLI login -->
            <template v-else-if="selectedAgent === 'codex' && loginMethod.codex === 'cli'">
              <p class="text-muted">{{ $t('agent.codex_cli_hint') }}</p>
              <div class="cli-cmd">codex --login</div>
              <p class="text-muted text-sm">{{ $t('agent.codex_cli_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Codex: API Key -->
            <template v-else-if="selectedAgent === 'codex' && loginMethod.codex === 'token'">
              <p class="text-muted">{{ $t('agent.codex_token_hint') }}</p>
              <div style="width: 100%">
                <div class="field">
                  <label class="field-label">api_key (OPENAI_API_KEY)</label>
                  <input type="password" v-model="data.configs.codex.api_key" class="input-mono" :placeholder="$t('agent.codex_key_placeholder')" />
                </div>
                <div class="flex-row-end mt-4">
                  <button class="btn btn-primary" @click="async () => {
                    try {
                      await api.saveAgentConfig({ codex: data.configs.codex })
                      addToast('success', t('agent.codex_saved'))
                      const refreshed = await api.getAgents()
                      Object.assign(data, refreshed)
                    } catch (e: any) { addToast('error', e.message) }
                  }">{{ $t('agent.save_continue') }}</button>
                </div>
              </div>
            </template>

            <!-- Gemini: Google account login -->
            <template v-else-if="selectedAgent === 'gemini' && loginMethod.gemini === 'cli'">
              <p class="text-muted">{{ $t('agent.gemini_hint') }}</p>
              <div class="cli-cmd">gemini</div>
              <p class="text-muted text-sm">{{ $t('agent.gemini_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Gemini: API Key -->
            <template v-else-if="selectedAgent === 'gemini' && loginMethod.gemini === 'token'">
              <p class="text-muted">{{ $t('agent.gemini_token_hint') }}</p>
              <div style="width: 100%">
                <div class="field">
                  <label class="field-label">api_key (GEMINI_API_KEY)</label>
                  <input type="password" v-model="data.configs.gemini.api_key" class="input-mono" :placeholder="$t('agent.gemini_key_placeholder')" />
                </div>
                <div class="flex-row-end mt-4">
                  <button class="btn btn-primary" @click="async () => {
                    try {
                      await api.saveAgentConfig({ gemini: data.configs.gemini })
                      addToast('success', t('agent.gemini_saved'))
                      const refreshed = await api.getAgents()
                      Object.assign(data, refreshed)
                    } catch (e: any) { addToast('error', e.message) }
                  }">{{ $t('agent.save_continue') }}</button>
                </div>
              </div>
            </template>

            <!-- Hermes: OAuth login -->
            <template v-else-if="selectedAgent === 'hermes'">
              <p class="text-muted">{{ $t('agent.hermes_hint') }}</p>
              <div class="cli-cmd">hermes setup --portal</div>
              <p class="text-muted text-sm">{{ $t('agent.hermes_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Kiro: AWS login -->
            <template v-else-if="selectedAgent === 'kiro'">
              <p class="text-muted">{{ $t('agent.kiro_hint') }}</p>
              <div class="cli-cmd">kiro-cli</div>
              <p class="text-muted text-sm">{{ $t('agent.kiro_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>

            <!-- Generic fallback -->
            <template v-else>
              <p class="text-muted">{{ $t('agent.generic_hint') }}</p>
              <p class="text-muted text-sm">{{ $t('agent.generic_post') }}</p>
              <button class="btn btn-ghost" @click="refreshAgents">{{ $t('agent.refresh_status') }}</button>
            </template>
          </div>

          <!-- Step 3: Ready -->
          <div v-else class="step-content">
            <div class="gap-row" style="margin-bottom: 16px">
              <span class="badge badge-green"><span class="badge-dot"></span>{{ $t('agent.step_ready') }}</span>
              <span v-if="getAgent(selectedAgent!)?.version" class="badge badge-muted">v{{ getAgent(selectedAgent!)?.version }}</span>
            </div>

            <button
              v-if="data.backend !== selectedAgent"
              class="btn btn-primary"
              @click="setActive(selectedAgent!)"
            >
              {{ $t('agent.set_active') }}
            </button>
            <span v-else class="badge badge-green" style="font-size: 13px">
              <span class="badge-dot"></span>{{ $t('agent.current_active') }}
            </span>

            <!-- Codex settings -->
            <div v-if="selectedAgent === 'codex'" class="agent-settings">
              <hr class="divider" />
              <h4 class="settings-title">{{ $t('agent.codex_settings') }}</h4>
              <div class="field">
                <label class="field-label">model</label>
                <input type="text" v-model="data.configs.codex.model" class="input-mono" :placeholder="$t('agent.codex_model_placeholder')" />
              </div>
              <div class="grid grid-2">
                <div class="field">
                  <label class="field-label">sandbox_mode</label>
                  <select v-model="data.configs.codex.sandbox_mode">
                    <option value="read-only">read-only</option>
                    <option value="workspace-write">workspace-write</option>
                    <option value="danger-full-access">danger-full-access</option>
                  </select>
                </div>
                <div class="field">
                  <label class="field-label">approval_mode</label>
                  <select v-model="data.configs.codex.approval_mode">
                    <option value="never">never</option>
                    <option value="on-request">on-request</option>
                    <option value="on-failure">on-failure</option>
                    <option value="untrusted">untrusted</option>
                  </select>
                </div>
              </div>
              <div class="flex-row-end mt-4">
                <button class="btn btn-sm btn-primary" @click="async () => {
                  try {
                    await api.saveAgentConfig({ codex: data.configs.codex })
                    addToast('success', t('agent.codex_saved'))
                  } catch (e: any) { addToast('error', e.message) }
                }">{{ $t('agent.save_settings') }}</button>
              </div>
            </div>

            <!-- OpenCode settings -->
            <div v-if="selectedAgent === 'opencode'" class="agent-settings">
              <hr class="divider" />
              <h4 class="settings-title">{{ $t('agent.opencode_settings') }}</h4>
              <div class="field">
                <label class="field-label">base_url</label>
                <input type="text" v-model="data.configs.opencode.base_url" class="input-mono" :placeholder="$t('agent.opencode_url_placeholder')" />
              </div>
              <div class="field">
                <label class="field-label">api_key</label>
                <input type="password" v-model="data.configs.opencode.api_key" class="input-mono" :placeholder="$t('agent.opencode_key_placeholder')" />
              </div>
              <div class="flex-row-end mt-4">
                <button class="btn btn-sm btn-primary" @click="async () => {
                  try {
                    await api.saveAgentConfig({ opencode: data.configs.opencode })
                    addToast('success', t('agent.opencode_saved'))
                  } catch (e: any) { addToast('error', e.message) }
                }">{{ $t('agent.save_settings') }}</button>
              </div>
            </div>

          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.login-header {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  margin-bottom: 4px;
}
.login-method-select {
  width: auto;
  min-width: 120px;
  padding: 4px 28px 4px 10px;
  font-size: 12px;
  margin-left: auto;
}
.cli-cmd {
  font: 13px/1.6 var(--font-mono);
  background: var(--bg-input);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 10px 16px;
  width: 100%;
  text-align: center;
  color: var(--green);
  user-select: all;
}
</style>
