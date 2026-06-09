<script setup lang="ts">
import { inject, reactive, watch, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import type { SettingsConfig } from '../types'
import { api } from '../api'
import { i18n } from '../i18n'
import { useRestart } from '../composables/useRestart'

const { t } = useI18n()
const addToast = inject<(type: 'success' | 'error' | 'info', msg: string) => void>('addToast')!
const { markDirty } = useRestart()

const settings = reactive<SettingsConfig>({
  bridge: { default_cwd: '', session_idle_timeout_secs: 7200, locale: 'zh-CN' },
  web: { bind: '127.0.0.1:7681' },
  proxy: { http: '', https: '', no_proxy: '' },
  log: { level: 'info' },
})

let savedSnapshot = ''
let restartSnapshot = ''
let saveTimer: ReturnType<typeof setTimeout> | null = null

function settingsSnapshot(): string {
  return JSON.stringify(settings)
}

async function fetchSettings() {
  try {
    const data = await api.getSettings()
    Object.assign(settings.bridge, data.bridge)
    Object.assign(settings.web, data.web)
    Object.assign(settings.proxy, data.proxy)
    Object.assign(settings.log, data.log)
    savedSnapshot = settingsSnapshot()
    restartSnapshot = JSON.stringify({ bridge: settings.bridge, web: settings.web, log: settings.log })
  } catch { /* ignore */ }
}

watch(settings, () => {
  if (!savedSnapshot) return
  const current = settingsSnapshot()
  if (current === savedSnapshot) return
  if (saveTimer) clearTimeout(saveTimer)
  saveTimer = setTimeout(async () => {
    try {
      await api.saveSettings(settings)
      const restartCurrent = JSON.stringify({ bridge: settings.bridge, web: settings.web, log: settings.log })
      if (restartCurrent !== restartSnapshot) {
        markDirty()
      }
      savedSnapshot = settingsSnapshot()
    } catch (e: any) {
      addToast('error', t('common.save_failed', { msg: e.message }))
    }
  }, 500)
}, { deep: true })

async function restart() {
  try {
    await api.restart()
    addToast('success', t('common.restarting'))
  } catch (e: any) {
    addToast('error', e.message)
  }
}

onMounted(fetchSettings)
</script>

<template>
  <!-- Session & Runtime -->
  <div class="card">
    <div class="card-head">
      <h3>{{ $t('settings.session_runtime') }}</h3>
    </div>
    <div class="card-body">
      <div class="field">
        <label class="field-label">{{ $t('settings.default_cwd') }}</label>
        <input
          type="text"
          v-model="settings.bridge.default_cwd"
          class="input-mono"
          :placeholder="$t('settings.default_cwd_placeholder')"
        />
        <p class="field-hint">{{ $t('settings.default_cwd_hint') }}</p>
      </div>
      <div class="grid grid-2">
        <div class="field">
          <label class="field-label">{{ $t('settings.idle_timeout') }}</label>
          <input
            type="number"
            v-model.number="settings.bridge.session_idle_timeout_secs"
            min="0"
          />
          <p class="field-hint">{{ $t('settings.idle_timeout_hint') }}</p>
        </div>
        <div class="field">
          <label class="field-label">{{ $t('settings.language') }}</label>
          <select v-model="settings.bridge.locale" @change="i18n.global.locale.value = settings.bridge.locale as any">
            <option value="zh-CN">{{ $t('settings.lang_zh') }}</option>
            <option value="en">{{ $t('settings.lang_en') }}</option>
          </select>
        </div>
      </div>
    </div>
  </div>

  <!-- Web -->
  <div class="card">
    <div class="card-head">
      <h3>{{ $t('settings.web_panel') }}</h3>
    </div>
    <div class="card-body">
      <div class="field">
        <label class="field-label">{{ $t('settings.bind_address') }}</label>
        <input type="text" v-model="settings.web.bind" class="input-mono" />
        <p class="field-hint">{{ $t('settings.bind_hint') }}</p>
      </div>
    </div>
  </div>

  <!-- Proxy -->
  <div class="card">
    <div class="card-head">
      <h3>{{ $t('settings.proxy') }}</h3>
    </div>
    <div class="card-body">
      <div class="grid grid-2">
        <div class="field">
          <label class="field-label">{{ $t('settings.http_proxy') }}</label>
          <input
            type="text"
            v-model="settings.proxy.http"
            class="input-mono"
            placeholder="http://proxy.example.com:8080"
          />
        </div>
        <div class="field">
          <label class="field-label">{{ $t('settings.https_proxy') }}</label>
          <input
            type="text"
            v-model="settings.proxy.https"
            class="input-mono"
            :placeholder="$t('settings.https_proxy_placeholder')"
          />
        </div>
      </div>
      <div class="field">
        <label class="field-label">{{ $t('settings.no_proxy') }}</label>
        <input
          type="text"
          v-model="settings.proxy.no_proxy"
          class="input-mono"
          placeholder="myhost.internal,.corp.example.com"
        />
        <p class="field-hint">{{ $t('settings.no_proxy_hint') }}</p>
      </div>
    </div>
  </div>

  <!-- Log -->
  <div class="card">
    <div class="card-head">
      <h3>{{ $t('settings.log_level') }}</h3>
    </div>
    <div class="card-body">
      <div class="field">
        <select v-model="settings.log.level">
          <option value="error">{{ $t('settings.log_error') }}</option>
          <option value="warn">{{ $t('settings.log_warn') }}</option>
          <option value="info">{{ $t('settings.log_info') }}</option>
          <option value="debug">{{ $t('settings.log_debug') }}</option>
          <option value="trace">{{ $t('settings.log_trace') }}</option>
        </select>
      </div>
    </div>
  </div>

  <!-- Danger zone -->
  <div class="card" style="border-color: rgba(248, 81, 73, 0.3)">
    <div class="card-head">
      <h3 style="color: var(--red)">{{ $t('settings.danger_zone') }}</h3>
    </div>
    <div class="card-body">
      <div style="display: flex; align-items: center; justify-content: space-between">
        <div>
          <div class="text-sm" style="font-weight: 500">{{ $t('settings.restart_agentline') }}</div>
          <div class="text-xs text-muted" style="margin-top: 2px">{{ $t('settings.restart_warning') }}</div>
        </div>
        <button class="btn btn-danger" @click="restart">{{ $t('settings.restart') }}</button>
      </div>
    </div>
  </div>

</template>
