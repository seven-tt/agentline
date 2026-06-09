<script setup lang="ts">
import { ref, computed, watch, provide, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ViewName } from './types'
import { useStatus } from './composables/useStatus'
import { useToast } from './composables/useToast'
import { useRestart } from './composables/useRestart'
import { api } from './api'
import { i18n } from './i18n'
import OverviewView from './views/OverviewView.vue'
import ChannelsView from './views/ChannelsView.vue'
import AgentView from './views/AgentView.vue'
import ProjectsView from './views/ProjectsView.vue'
import SettingsView from './views/SettingsView.vue'
import LogsView from './views/LogsView.vue'

const { t } = useI18n()
const { overview } = useStatus()
const { toasts, addToast } = useToast()
const restart = useRestart()

provide('overview', overview)
provide('addToast', addToast)

onMounted(async () => {
  try {
    const settings = await api.getSettings()
    if (settings.bridge.locale) {
      i18n.global.locale.value = settings.bridge.locale as 'zh-CN' | 'en'
    }
  } catch { /* ignore */ }
})

const view = ref<ViewName>('overview')

// Trigger restart dialog when navigating away from a config page
watch(view, () => {
  restart.triggerDialog()
})

// Trigger restart dialog after 30s idle since last config change
let idleTimer: ReturnType<typeof setTimeout> | null = null
watch(() => restart.lastModifiedAt.value, (ts) => {
  if (idleTimer) clearTimeout(idleTimer)
  if (ts > 0) {
    idleTimer = setTimeout(() => restart.triggerDialog(), 30000)
  }
})

async function doRestart() {
  restart.clearRestart()
  try {
    await api.restart()
    addToast('success', t('common.restarting'))
  } catch (e: any) {
    addToast('error', e.message)
  }
}

const navItems: { id: ViewName; labelKey: string; descKey: string; icon: string }[] = [
  { id: 'overview', labelKey: 'nav.overview', descKey: 'nav.overview_desc',
    icon: '<path d="M0 1.75A.75.75 0 01.75 1h4.5a.75.75 0 01.75.75v4.5a.75.75 0 01-.75.75H.75A.75.75 0 010 6.25v-4.5zm7 0A.75.75 0 017.75 1h4.5a.75.75 0 01.75.75v4.5a.75.75 0 01-.75.75h-4.5A.75.75 0 017 6.25v-4.5zM0 9.75A.75.75 0 01.75 9h4.5a.75.75 0 01.75.75v4.5a.75.75 0 01-.75.75H.75A.75.75 0 010 14.25v-4.5zm7 0A.75.75 0 017.75 9h4.5a.75.75 0 01.75.75v4.5a.75.75 0 01-.75.75h-4.5a.75.75 0 01-.75-.75v-4.5z"/>' },
  { id: 'channels', labelKey: 'nav.channels', descKey: 'nav.channels_desc',
    icon: '<path d="M1.5 2.75a.25.25 0 01.25-.25h12.5a.25.25 0 01.25.25v8.5a.25.25 0 01-.25.25h-6.5a.75.75 0 00-.53.22L4.5 14.44v-2.19a.75.75 0 00-.75-.75h-2a.25.25 0 01-.25-.25v-8.5zM1.75 1A1.75 1.75 0 000 2.75v8.5C0 12.22.78 13 1.75 13H3v1.543a1.457 1.457 0 002.487 1.03L8.61 13h5.64c.97 0 1.75-.78 1.75-1.75v-8.5A1.75 1.75 0 0014.25 1H1.75z"/>' },
  { id: 'agent', labelKey: 'nav.agent', descKey: 'nav.agent_desc',
    icon: '<path d="M8.75 1a.75.75 0 00-1.5 0v1.5h-3A1.75 1.75 0 002.5 4.25v.5H1a.75.75 0 000 1.5h1.5v2H1a.75.75 0 000 1.5h1.5v2H1a.75.75 0 000 1.5h1.5v.5c0 .97.78 1.75 1.75 1.75h8.5c.97 0 1.75-.78 1.75-1.75V4.25c0-.97-.78-1.75-1.75-1.75h-3V1h-1.5zm-1.5 3h-3a.25.25 0 00-.25.25v9.5c0 .14.11.25.25.25h8.5a.25.25 0 00.25-.25v-9.5a.25.25 0 00-.25-.25h-5.5zM6 7a1 1 0 100-2 1 1 0 000 2zm4-1a1 1 0 11-2 0 1 1 0 012 0zm-5.25 4a.75.75 0 000 1.5h6.5a.75.75 0 000-1.5h-6.5z"/>' },
  { id: 'projects', labelKey: 'nav.projects', descKey: 'nav.projects_desc',
    icon: '<path d="M1.75 1A1.75 1.75 0 000 2.75v10.5C0 14.22.78 15 1.75 15h12.5c.97 0 1.75-.78 1.75-1.75v-8.5A1.75 1.75 0 0014.25 3H7.5a.25.25 0 01-.2-.1l-.9-1.2c-.33-.44-.85-.7-1.4-.7H1.75z"/>' },
  { id: 'settings', labelKey: 'nav.settings', descKey: 'nav.settings_desc',
    icon: '<path fill-rule="evenodd" d="M6.69.89a1.06 1.06 0 012.62 0 .63.63 0 00.87.5 1.06 1.06 0 011.86 1.07.63.63 0 00.5.87 1.06 1.06 0 010 2.62.63.63 0 00-.5.87 1.06 1.06 0 01-1.07 1.86.63.63 0 00-.87.5 1.06 1.06 0 01-2.62 0 .63.63 0 00-.87-.5A1.06 1.06 0 013.96 6.82a.63.63 0 00-.5-.87 1.06 1.06 0 010-2.62.63.63 0 00.5-.87A1.06 1.06 0 015.03 1.4a.63.63 0 00.87-.5H6.69zM8 5.5a2.5 2.5 0 100 5 2.5 2.5 0 000-5zM6.75 8a1.25 1.25 0 112.5 0 1.25 1.25 0 01-1.25 1.25A1.25 1.25 0 016.75 8z"/>' },
  { id: 'logs', labelKey: 'nav.logs', descKey: 'nav.logs_desc',
    icon: '<path d="M0 2.75C0 1.78.78 1 1.75 1h12.5c.97 0 1.75.78 1.75 1.75v10.5A1.75 1.75 0 0114.25 15H1.75A1.75 1.75 0 010 13.25V2.75zm1.75-.25a.25.25 0 00-.25.25v10.5c0 .14.11.25.25.25h12.5a.25.25 0 00.25-.25V2.75a.25.25 0 00-.25-.25H1.75zM3.5 6.25a.75.75 0 01.75-.75h7.5a.75.75 0 010 1.5h-7.5a.75.75 0 01-.75-.75zm.75 2.25a.75.75 0 000 1.5h4.5a.75.75 0 000-1.5h-4.5z"/>' },
]

const currentNav = computed(() => navItems.find((n) => n.id === view.value)!)

const viewComponents: Record<ViewName, any> = {
  overview: OverviewView,
  channels: ChannelsView,
  agent: AgentView,
  projects: ProjectsView,
  settings: SettingsView,
  logs: LogsView,
}

const currentComponent = computed(() => viewComponents[view.value])
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="sidebar-brand">
        <div class="brand-mark"><svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#1A1F36"/><polygon points="13,7 20,10.5 20,17.5 13,21 6,17.5 6,10.5" fill="#4F6BFF" stroke="#6B82FF" stroke-width="0.8"/><circle cx="13" cy="14" r="2.5" fill="#fff"/><circle cx="13" cy="14" r="1" fill="#4F6BFF"/><line x1="20.5" y1="14" x2="28" y2="14" stroke="#00D4FF" stroke-width="1.5" stroke-linecap="round"/><line x1="19" y1="10" x2="25" y2="7" stroke="#00D4FF" stroke-width="1" stroke-linecap="round" opacity="0.7"/><line x1="19" y1="18" x2="25" y2="21" stroke="#00D4FF" stroke-width="1" stroke-linecap="round" opacity="0.7"/><circle cx="26" cy="14" r="1.5" fill="#00D4FF"/><circle cx="24" cy="7.5" r="1" fill="#00D4FF" opacity="0.7"/><circle cx="24" cy="20.5" r="1" fill="#00D4FF" opacity="0.7"/></svg></div>
        <span class="brand-name">agentline</span>
      </div>
      <nav class="sidebar-nav">
        <button
          v-for="item in navItems"
          :key="item.id"
          :class="['nav-item', { active: view === item.id }]"
          @click="view = item.id"
        >
          <svg viewBox="0 0 16 16" fill="currentColor" v-html="item.icon"></svg>
          <span>{{ $t(item.labelKey) }}</span>
        </button>
      </nav>
      <div class="sidebar-footer">
        <span
          class="status-dot"
          :class="overview.ims.some(im => im.healthy) ? 'online' : 'offline'"
        ></span>
        <span>{{ overview.version ? `v${overview.version}` : '...' }}</span>
      </div>
    </aside>

    <div class="main">
      <header class="page-header">
        <h1>{{ $t(currentNav.labelKey) }}</h1>
        <p v-if="currentNav.descKey">{{ $t(currentNav.descKey) }}</p>
      </header>
      <div class="page-body">
        <keep-alive>
          <component :is="currentComponent" />
        </keep-alive>
      </div>
    </div>
  </div>

  <Teleport to="body">
    <div v-if="restart.showDialog.value" class="modal-overlay" @click.self="restart.dismiss()">
      <div class="modal" style="max-width: 400px">
        <div class="modal-header" style="border-bottom: none; padding-bottom: 0">
          <div class="modal-header-info">
            <div class="modal-title">{{ $t('restart_dialog.title') }}</div>
          </div>
          <button class="modal-close" @click="restart.dismiss()">&times;</button>
        </div>
        <div class="modal-body">
          <p class="text-muted" style="margin-bottom: 20px">{{ $t('restart_dialog.message') }}</p>
          <div style="display: flex; gap: 8px; justify-content: flex-end">
            <button class="btn btn-ghost" @click="restart.dismiss()">{{ $t('restart_dialog.later') }}</button>
            <button class="btn btn-primary" @click="doRestart">{{ $t('restart_dialog.now') }}</button>
          </div>
        </div>
      </div>
    </div>
  </Teleport>

  <div class="toast-container">
    <div
      v-for="t in toasts"
      :key="t.id"
      :class="['toast', 'toast-' + t.type]"
    >
      <svg
        width="14"
        height="14"
        viewBox="0 0 16 16"
        fill="currentColor"
        :style="{
          color:
            t.type === 'success'
              ? 'var(--green)'
              : t.type === 'error'
                ? 'var(--red)'
                : 'var(--blue)',
        }"
      >
        <path
          v-if="t.type === 'success'"
          d="M8 16A8 8 0 108 0a8 8 0 000 16zm3.78-9.72a.75.75 0 00-1.06-1.06L7 8.94 5.28 7.22a.75.75 0 00-1.06 1.06l2.25 2.25a.75.75 0 001.06 0l4.25-4.25z"
        />
        <path
          v-else
          d="M2.343 13.657A8 8 0 1113.657 2.343 8 8 0 012.343 13.657zM6.03 4.97a.75.75 0 00-1.06 1.06L6.94 8 4.97 9.97a.75.75 0 101.06 1.06L8 9.06l1.97 1.97a.75.75 0 101.06-1.06L9.06 8l1.97-1.97a.75.75 0 10-1.06-1.06L8 6.94 6.03 4.97z"
        />
      </svg>
      {{ t.message }}
    </div>
  </div>
</template>

<style>
:root {
  --bg-primary: #0d1117;
  --bg-secondary: #161b22;
  --bg-tertiary: #1c2128;
  --bg-input: #0d1117;
  --text-primary: #e6edf3;
  --text-secondary: #8b949e;
  --text-tertiary: #6e7681;
  --border: #30363d;
  --border-muted: #21262d;
  --green: #3fb950;
  --blue: #58a6ff;
  --amber: #d29922;
  --red: #f85149;
  --green-muted: rgba(63, 185, 80, 0.12);
  --blue-muted: rgba(88, 166, 255, 0.12);
  --amber-muted: rgba(210, 153, 34, 0.12);
  --red-muted: rgba(248, 81, 73, 0.12);
  --font-sans: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui,
    sans-serif;
  --font-mono: 'JetBrains Mono', 'SF Mono', ui-monospace, Menlo, monospace;
  --radius: 8px;
  --sidebar-w: 228px;
}
* {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}
html,
body {
  height: 100%;
}
body {
  font: 14px/1.55 var(--font-sans);
  color: var(--text-primary);
  background: var(--bg-primary);
  -webkit-font-smoothing: antialiased;
}

.shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
}
.sidebar {
  width: var(--sidebar-w);
  background: var(--bg-secondary);
  border-right: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  flex-shrink: 0;
}
.sidebar-brand {
  padding: 20px 16px;
  display: flex;
  align-items: center;
  gap: 10px;
  border-bottom: 1px solid var(--border);
}
.brand-mark {
  width: 28px;
  height: 28px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  overflow: hidden;
  flex-shrink: 0;
}
.brand-mark svg {
  width: 100%;
  height: 100%;
}
.brand-name {
  font: 600 15px/1 var(--font-mono);
  letter-spacing: -0.02em;
}
.sidebar-nav {
  flex: 1;
  padding: 8px;
  overflow-y: auto;
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  border-radius: 6px;
  color: var(--text-secondary);
  cursor: pointer;
  transition: all 0.15s;
  font-size: 13px;
  user-select: none;
  border: none;
  background: none;
  width: 100%;
  text-align: left;
}
.nav-item:hover {
  background: rgba(255, 255, 255, 0.04);
  color: var(--text-primary);
}
.nav-item.active {
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-primary);
}
.nav-item svg {
  width: 16px;
  height: 16px;
  opacity: 0.6;
  flex-shrink: 0;
}
.nav-item.active svg {
  opacity: 1;
}
.sidebar-footer {
  padding: 14px 16px;
  border-top: 1px solid var(--border);
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-tertiary);
}
.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.status-dot.online {
  background: var(--green);
  box-shadow: 0 0 6px var(--green);
}
.status-dot.offline {
  background: var(--text-tertiary);
}
.main {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  min-width: 0;
}
.page-header {
  padding: 20px 32px 16px;
  border-bottom: 1px solid var(--border);
}
.page-header h1 {
  font-size: 17px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.page-header p {
  font-size: 13px;
  color: var(--text-secondary);
  margin-top: 2px;
}
.page-body {
  flex: 1;
  overflow-y: auto;
  padding: 24px 32px 80px;
}

/* Card */
.card {
  background: var(--bg-secondary);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  overflow: hidden;
}
.card + .card {
  margin-top: 16px;
}
.grid > .card + .card {
  margin-top: 0;
}
.card-head {
  padding: 16px 20px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.card-head h3 {
  font-size: 14px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 10px;
}
.card-head-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}
.card-body {
  padding: 0 20px 20px;
}
.card-body.bordered {
  border-top: 1px solid var(--border);
  padding-top: 16px;
}

/* Grid */
.grid {
  display: grid;
  gap: 16px;
}
.grid-2 {
  grid-template-columns: repeat(2, 1fr);
}
.grid-3 {
  grid-template-columns: repeat(3, 1fr);
}
@media (max-width: 900px) {
  .grid-2,
  .grid-3 {
    grid-template-columns: 1fr;
  }
}

/* Form */
.field {
  margin-bottom: 16px;
}
.field:last-child {
  margin-bottom: 0;
}
.field-label {
  display: block;
  font-size: 12px;
  font-weight: 500;
  color: var(--text-secondary);
  margin-bottom: 6px;
  letter-spacing: 0.02em;
}
.field-hint {
  font-size: 11px;
  color: var(--text-tertiary);
  margin-top: 4px;
}
input[type='text'],
input[type='password'],
input[type='number'],
select,
textarea {
  width: 100%;
  background: var(--bg-input);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-primary);
  padding: 8px 12px;
  font: 13px var(--font-sans);
  transition: border-color 0.15s;
  outline: none;
}
input:focus,
select:focus,
textarea:focus {
  border-color: var(--blue);
}
input::placeholder,
textarea::placeholder {
  color: var(--text-tertiary);
}
select {
  appearance: none;
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' fill='%238b949e'%3E%3Cpath d='M6 8.5L1.5 4h9z'/%3E%3C/svg%3E");
  background-repeat: no-repeat;
  background-position: right 10px center;
  padding-right: 32px;
  cursor: pointer;
}
textarea {
  resize: vertical;
  min-height: 60px;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
}
.input-mono {
  font-family: var(--font-mono);
  font-size: 12px;
}

/* Toggle */
.toggle {
  position: relative;
  width: 40px;
  height: 22px;
  background: var(--border);
  border: none;
  border-radius: 11px;
  cursor: pointer;
  transition: background 0.2s;
  padding: 0;
  flex-shrink: 0;
}
.toggle.on {
  background: var(--green);
}
.toggle .thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 18px;
  height: 18px;
  background: #fff;
  border-radius: 50%;
  transition: transform 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  pointer-events: none;
}
.toggle.on .thumb {
  transform: translateX(18px);
}

/* Buttons */
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 7px 14px;
  border-radius: 6px;
  font: 500 13px var(--font-sans);
  cursor: pointer;
  transition: all 0.15s;
  border: 1px solid transparent;
  user-select: none;
}
.btn-primary {
  background: #238636;
  color: #fff;
  border-color: rgba(240, 246, 252, 0.1);
}
.btn-primary:hover {
  background: #2ea043;
}
.btn-primary:disabled {
  opacity: 0.5;
  cursor: default;
}
.btn-ghost {
  background: transparent;
  color: var(--text-secondary);
  border-color: var(--border);
}
.btn-ghost:hover {
  color: var(--text-primary);
  background: rgba(255, 255, 255, 0.04);
}
.btn-danger {
  background: transparent;
  color: var(--red);
  border-color: var(--border);
}
.btn-danger:hover {
  background: var(--red-muted);
}
.btn-sm {
  padding: 4px 10px;
  font-size: 12px;
}

/* Badge */
.badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 10px;
  border-radius: 20px;
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.02em;
}
.badge-green {
  background: var(--green-muted);
  color: var(--green);
}
.badge-amber {
  background: var(--amber-muted);
  color: var(--amber);
}
.badge-red {
  background: var(--red-muted);
  color: var(--red);
}
.badge-muted {
  background: rgba(139, 148, 158, 0.1);
  color: var(--text-secondary);
}
.badge-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  display: inline-block;
}
.badge-green .badge-dot {
  background: var(--green);
}

/* IM dots — color set dynamically via inline style */
.im-dot {
  width: 10px;
  height: 10px;
  border-radius: 3px;
  flex-shrink: 0;
  background: var(--text-secondary);
}

/* Stat */
.stat {
  padding: 16px 20px;
}
.stat-value {
  font: 600 28px/1.1 var(--font-mono);
  letter-spacing: -0.02em;
  margin-bottom: 4px;
  font-variant-numeric: tabular-nums;
}
.stat-label {
  font-size: 12px;
  color: var(--text-secondary);
}

/* Save bar */
.save-bar {
  position: fixed;
  bottom: 0;
  left: var(--sidebar-w);
  right: 0;
  background: var(--bg-tertiary);
  border-top: 1px solid var(--border);
  padding: 12px 32px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  z-index: 100;
  transform: translateY(0);
  transition: transform 0.25s cubic-bezier(0.4, 0, 0.2, 1);
}
.save-bar.hidden {
  transform: translateY(100%);
  pointer-events: none;
}
.save-bar span {
  font-size: 13px;
  color: var(--text-secondary);
}
.save-bar-actions {
  display: flex;
  gap: 8px;
}

/* Toast */
.toast-container {
  position: fixed;
  top: 16px;
  right: 16px;
  z-index: 200;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.toast {
  padding: 10px 16px;
  border-radius: var(--radius);
  font-size: 13px;
  display: flex;
  align-items: center;
  gap: 8px;
  animation: toastIn 0.3s ease;
  min-width: 240px;
  border: 1px solid var(--border);
  background: var(--bg-tertiary);
}
.toast-success {
  border-color: rgba(63, 185, 80, 0.3);
}
.toast-error {
  border-color: rgba(248, 81, 73, 0.3);
}
@keyframes toastIn {
  from {
    opacity: 0;
    transform: translateX(20px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}

/* Utility */
.text-muted {
  color: var(--text-secondary);
}
.text-mono {
  font-family: var(--font-mono);
}
.text-sm {
  font-size: 12px;
}
.text-xs {
  font-size: 11px;
}
.mt-4 {
  margin-top: 16px;
}
.mt-2 {
  margin-top: 8px;
}
.mb-4 {
  margin-bottom: 16px;
}
.gap-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.flex-row-end {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.divider {
  border: none;
  border-top: 1px solid var(--border);
  margin: 16px 0;
}
.empty-state {
  padding: 48px 20px;
  text-align: center;
  color: var(--text-tertiary);
  font-size: 13px;
}

/* Agent */
.agent-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
}
.agent-card {
  background: var(--bg-secondary);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 20px;
  cursor: pointer;
  transition: all 0.15s;
}
.agent-card:hover {
  border-color: var(--blue);
  background: rgba(88, 166, 255, 0.03);
}
.agent-card.is-active {
  border-color: var(--green);
}
.agent-card-head {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}
.agent-card-name {
  font: 600 14px var(--font-sans);
  flex: 1;
}
.agent-card-tag {
  font-size: 9px;
  font-weight: 700;
  color: var(--green);
  letter-spacing: 0.06em;
  text-transform: uppercase;
  background: var(--green-muted);
  padding: 2px 6px;
  border-radius: 3px;
}
.agent-card-tag.tag-gray {
  color: var(--text-secondary);
  background: rgba(139, 148, 158, 0.1);
}
.agent-card-tag.tag-blue {
  color: var(--blue);
  background: rgba(88, 166, 255, 0.12);
}
.agent-card.is-not-installed {
  opacity: 0.5;
}
.badge-blue {
  background: rgba(88, 166, 255, 0.12);
  color: var(--blue);
}
.agent-card-desc {
  font-size: 12px;
  color: var(--text-secondary);
  line-height: 1.5;
  margin-bottom: 14px;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.agent-card-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.agent-icon {
  width: 32px;
  height: 32px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  font: bold 13px var(--font-mono);
  color: #fff;
  flex-shrink: 0;
}
.agent-icon.claude-code {
  background: linear-gradient(135deg, #d97706, #b45309);
}
.agent-icon.kimi {
  background: linear-gradient(135deg, #7c3aed, #5b21b6);
}
.agent-icon.qoder {
  background: linear-gradient(135deg, #0891b2, #0e7490);
}
.agent-icon.opencode {
  background: linear-gradient(135deg, #059669, #047857);
}
.agent-icon.kiro {
  background: linear-gradient(135deg, #f97316, #ea580c);
}
.agent-icon.gemini {
  background: linear-gradient(135deg, #4285f4, #1a73e8);
}
.agent-icon.codex {
  background: linear-gradient(135deg, #10a37f, #0d8a6a);
}
.agent-icon.acp {
  background: linear-gradient(135deg, #6b7280, #4b5563);
}

/* Modal */
.modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(4px);
  z-index: 2000;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  animation: fadeIn 0.15s ease;
}
.modal {
  background: var(--bg-secondary);
  border: 1px solid var(--border);
  border-radius: 12px;
  width: 100%;
  max-width: 560px;
  max-height: calc(100vh - 48px);
  overflow-y: auto;
  box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
  animation: slideUp 0.2s ease;
}
.modal-header {
  padding: 20px 24px 16px;
  display: flex;
  align-items: center;
  gap: 14px;
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--bg-secondary);
  z-index: 1;
  border-radius: 12px 12px 0 0;
}
.modal-header-info {
  flex: 1;
  min-width: 0;
}
.modal-title {
  font: 600 17px var(--font-sans);
  margin-bottom: 2px;
}
.modal-desc {
  font-size: 13px;
  color: var(--text-secondary);
  line-height: 1.5;
}
.modal-close {
  width: 32px;
  height: 32px;
  background: none;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-secondary);
  cursor: pointer;
  display: grid;
  place-items: center;
  font-size: 16px;
  transition: all 0.15s;
  flex-shrink: 0;
}
.modal-close:hover {
  background: rgba(255, 255, 255, 0.06);
  color: var(--text-primary);
}
.modal-body {
  padding: 20px 24px 24px;
}

/* Agent card link */
.agent-card-link {
  font-size: 12px;
  color: var(--text-secondary);
  cursor: pointer;
}
.agent-card:hover .agent-card-link {
  color: var(--text-primary);
}

/* Stepper */
.stepper {
  display: flex;
  align-items: center;
  gap: 0;
  padding: 16px 0;
  margin-bottom: 20px;
  border-bottom: 1px solid var(--border);
}
.step {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--text-secondary);
  font-size: 13px;
}
.step.active {
  color: var(--green);
}
.step.current {
  color: var(--text-primary);
}
.step-num {
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 1.5px solid currentColor;
  display: grid;
  place-items: center;
  font-size: 11px;
  font-weight: 600;
  flex-shrink: 0;
}
.step.active .step-num {
  background: var(--green);
  border-color: var(--green);
  color: #fff;
}
.step-label {
  font-weight: 500;
}
.step-line {
  flex: 1;
  height: 1px;
  background: var(--border);
  margin: 0 12px;
}
.step-line.done {
  background: var(--green);
}

/* Step content */
.step-content {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
  padding: 24px 0;
  text-align: center;
}
.step-title {
  font-size: 15px;
  font-weight: 600;
  margin: 0;
}
.link-muted {
  font-size: 13px;
  color: var(--text-secondary);
  text-decoration: none;
}
.link-muted:hover {
  color: var(--text-primary);
}

/* Agent settings block */
.agent-settings {
  width: 100%;
  margin-top: 16px;
}
.settings-title {
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 12px;
}
@keyframes fadeIn {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}
@keyframes slideUp {
  from {
    opacity: 0;
    transform: translateY(12px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* Log */
.log-viewer {
  background: var(--bg-input);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  font: 12px/1.7 var(--font-mono);
  overflow: auto;
  max-height: 480px;
  padding: 12px 0;
}
.log-line {
  padding: 0 16px;
  display: flex;
  gap: 12px;
  white-space: nowrap;
}
.log-line:hover {
  background: rgba(255, 255, 255, 0.02);
}
.log-time {
  color: var(--text-tertiary);
  flex-shrink: 0;
}
.log-level {
  width: 44px;
  flex-shrink: 0;
  text-transform: uppercase;
  font-weight: 500;
  font-size: 11px;
  letter-spacing: 0.04em;
  line-height: 1.7;
}
.log-level.info {
  color: var(--blue);
}
.log-level.warn {
  color: var(--amber);
}
.log-level.error {
  color: var(--red);
}
.log-level.debug {
  color: var(--text-tertiary);
}
.log-source {
  color: var(--text-secondary);
  flex-shrink: 0;
}
.log-msg {
  color: var(--text-primary);
  overflow: hidden;
  text-overflow: ellipsis;
}
.log-filters {
  display: flex;
  gap: 6px;
  margin-bottom: 12px;
}
.log-filter {
  padding: 4px 10px;
  border-radius: 20px;
  font-size: 11px;
  font-weight: 500;
  background: transparent;
  border: 1px solid var(--border);
  color: var(--text-secondary);
  cursor: pointer;
  transition: all 0.15s;
}
.log-filter.active {
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-primary);
  border-color: rgba(255, 255, 255, 0.15);
}

/* Projects */
.project-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 0;
  border-bottom: 1px solid var(--border-muted);
}
.project-row:last-child {
  border-bottom: none;
}
.project-name {
  font: 500 13px var(--font-mono);
  color: var(--blue);
  min-width: 120px;
}
.project-url {
  font: 12px var(--font-mono);
  color: var(--text-secondary);
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Callout */
.callout {
  padding: 12px 16px;
  border-radius: 6px;
  font-size: 12px;
  color: var(--text-secondary);
  border: 1px solid var(--border-muted);
  background: var(--bg-tertiary);
  margin-bottom: 16px;
  display: flex;
  align-items: center;
  gap: 10px;
}

/* IM card */
.im-card-header {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 16px 20px;
  border-bottom: 1px solid var(--border-muted);
}
.im-card-header .im-name {
  font-size: 14px;
  font-weight: 600;
  flex: 1;
}

@media (max-width: 768px) {
  .sidebar {
    display: none;
  }
  .save-bar {
    left: 0;
  }
}
</style>
