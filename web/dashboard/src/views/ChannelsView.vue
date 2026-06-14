<script setup lang="ts">
import { inject, ref, reactive, watch, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ChannelsConfig, LoginStatus, OverviewData } from '../types'
import { api } from '../api'
import { useRestart } from '../composables/useRestart'

const { t } = useI18n()
const addToast = inject<(type: 'success' | 'error' | 'info', msg: string) => void>('addToast')!
const overview = inject<OverviewData>('overview')!

function imDotColor(id: string, enabled: boolean): string {
  if (!enabled) return 'var(--text-secondary)'
  const im = overview.ims.find(i => i.id === id)
  if (!im || !im.healthy) return 'var(--red)'
  return 'var(--green)'
}
const { markDirty } = useRestart()

const channels = reactive<ChannelsConfig>({
  wechat: { enable: false, allowed_users: [], typing_interval_ms: 5000, logged_in: false },
  dingtalk: { enable: false, client_id: '', client_secret: '', allowed_users: [] },
  feishu: {
    enable: false,
    app_id: '',
    app_secret: '',
    allowed_users: [],
  },
  telegram: { enable: false, bot_token: '', api_base: '', allowed_users: [] },
})

let savedSnapshot = ''
let initialSnapshot = ''
let saveTimer: ReturnType<typeof setTimeout> | null = null

function channelsSnapshot(): string {
  const { logged_in, ...wc } = channels.wechat
  return JSON.stringify({ wechat: wc, dingtalk: channels.dingtalk, feishu: channels.feishu, telegram: channels.telegram })
}

async function fetchChannels() {
  try {
    const data = await api.getChannels()
    Object.assign(channels.wechat, data.wechat)
    Object.assign(channels.dingtalk, data.dingtalk)
    Object.assign(channels.feishu, data.feishu)
    Object.assign(channels.telegram, data.telegram)
    if (data.wechat.logged_in) {
      loginState.value = { state: 'completed', message: '' }
    }
    savedSnapshot = channelsSnapshot()
    initialSnapshot = savedSnapshot
  } catch { /* ignore */ }
}

watch(channels, () => {
  if (!savedSnapshot) return
  const current = channelsSnapshot()
  if (current === savedSnapshot) return
  if (saveTimer) clearTimeout(saveTimer)
  saveTimer = setTimeout(async () => {
    try {
      await api.saveChannels(channels)
      savedSnapshot = channelsSnapshot()
      if (savedSnapshot !== initialSnapshot) {
        markDirty()
      }
    } catch (e: any) {
      addToast('error', t('common.save_failed', { msg: e.message }))
    }
  }, 500)
}, { deep: true })

// WeChat login
const loginState = ref<LoginStatus>({ state: 'idle', message: '' })
let loginPoll: ReturnType<typeof setInterval> | null = null

async function startLogin() {
  try {
    await api.wechatLoginStart()
    loginState.value = { state: 'starting', message: '' }
    loginPoll = setInterval(async () => {
      loginState.value = await api.wechatLoginStatus()
      if (loginState.value.state === 'completed' || loginState.value.state === 'failed') {
        if (loginPoll) clearInterval(loginPoll)
        loginPoll = null
        if (loginState.value.state === 'completed') {
          addToast('success', t('channels.wechat_login_success'))
        } else {
          addToast('error', t('common.login_failed', { msg: loginState.value.message }))
        }
      }
    }, 1500)
  } catch (e: any) {
    addToast('error', e.message)
  }
}

async function cancelLogin() {
  await api.wechatLoginCancel()
  if (loginPoll) clearInterval(loginPoll)
  loginPoll = null
  loginState.value = { state: 'idle', message: '' }
}

onMounted(fetchChannels)
</script>

<template>
  <!-- Feishu -->
  <div class="card">
    <div class="card-head">
      <h3><span class="im-dot" :style="{ background: imDotColor('feishu', channels.feishu.enable) }"></span> {{ $t('channels.feishu_title') }}</h3>
      <div class="card-head-actions">
        <span v-if="channels.feishu.enable" class="badge badge-green"><span class="badge-dot"></span>{{ $t('common.enabled') }}</span>
        <button
          :class="['toggle', { on: channels.feishu.enable }]"
          @click="channels.feishu.enable = !channels.feishu.enable"
        >
          <span class="thumb"></span>
        </button>
      </div>
    </div>
    <div class="card-body" v-if="channels.feishu.enable">
      <div class="grid grid-2">
        <div class="field">
          <label class="field-label">app_id</label>
          <input type="text" v-model="channels.feishu.app_id" class="input-mono" :placeholder="$t('channels.feishu_app_id_placeholder')" />
        </div>
        <div class="field">
          <label class="field-label">app_secret</label>
          <input type="password" v-model="channels.feishu.app_secret" class="input-mono" :placeholder="$t('channels.feishu_app_secret_placeholder')" />
        </div>
      </div>
      <div class="field">
        <label class="field-label">allowed_users</label>
        <input
          type="text"
          :value="channels.feishu.allowed_users.join(', ')"
          @input="channels.feishu.allowed_users = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
          class="input-mono"
          :placeholder="$t('channels.allowed_users_comma_placeholder')"
        />
        <span class="field-hint">{{ $t('channels.feishu_allowed_hint') }}</span>
      </div>
    </div>
  </div>

  <!-- DingTalk -->
  <div class="card">
    <div class="card-head">
      <h3><span class="im-dot" :style="{ background: imDotColor('dingtalk', channels.dingtalk.enable) }"></span> {{ $t('channels.dingtalk_title') }}</h3>
      <div class="card-head-actions">
        <span v-if="channels.dingtalk.enable" class="badge badge-green"><span class="badge-dot"></span>{{ $t('common.enabled') }}</span>
        <button
          :class="['toggle', { on: channels.dingtalk.enable }]"
          @click="channels.dingtalk.enable = !channels.dingtalk.enable"
        >
          <span class="thumb"></span>
        </button>
      </div>
    </div>
    <div class="card-body" v-if="channels.dingtalk.enable">
      <div class="grid grid-2">
        <div class="field">
          <label class="field-label">client_id</label>
          <input type="text" v-model="channels.dingtalk.client_id" class="input-mono" placeholder="appKey" />
          <span class="field-hint">{{ $t('channels.dingtalk_client_id_hint') }}</span>
        </div>
        <div class="field">
          <label class="field-label">client_secret</label>
          <input type="password" v-model="channels.dingtalk.client_secret" class="input-mono" placeholder="appSecret" />
          <span class="field-hint">{{ $t('channels.dingtalk_client_secret_hint') }}</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">allowed_users</label>
        <input
          type="text"
          :value="channels.dingtalk.allowed_users.join(', ')"
          @input="channels.dingtalk.allowed_users = ($event.target as HTMLInputElement).value.split(',').map(s => s.trim()).filter(Boolean)"
          class="input-mono"
          :placeholder="$t('channels.allowed_users_comma_placeholder')"
        />
        <span class="field-hint">{{ $t('channels.dingtalk_allowed_hint') }}</span>
      </div>
    </div>
  </div>

  <!-- Telegram -->
  <div class="card">
    <div class="card-head">
      <h3><span class="im-dot" :style="{ background: imDotColor('telegram', channels.telegram.enable) }"></span> Telegram</h3>
      <div class="card-head-actions">
        <span v-if="channels.telegram.enable" class="badge badge-green"><span class="badge-dot"></span>{{ $t('common.enabled') }}</span>
        <button
          :class="['toggle', { on: channels.telegram.enable }]"
          @click="channels.telegram.enable = !channels.telegram.enable"
        >
          <span class="thumb"></span>
        </button>
      </div>
    </div>
    <div class="card-body" v-if="channels.telegram.enable">
      <div class="field">
        <label class="field-label">Bot Token</label>
        <input
          type="password"
          v-model="channels.telegram.bot_token"
          class="input-mono"
          :placeholder="$t('channels.telegram_token_placeholder')"
        />
      </div>
      <div class="field">
        <label class="field-label">API Base URL</label>
        <input
          type="text"
          v-model="channels.telegram.api_base"
          class="input-mono"
          :placeholder="$t('channels.telegram_api_placeholder')"
        />
      </div>
      <div class="field">
        <label class="field-label">{{ $t('channels.allowed_users') }}</label>
        <textarea
          class="input-mono"
          :value="channels.telegram.allowed_users.join('\n')"
          @input="channels.telegram.allowed_users = ($event.target as HTMLTextAreaElement).value.split('\n').filter(Boolean)"
          :placeholder="$t('channels.allowed_users_placeholder')"
        ></textarea>
      </div>
    </div>
  </div>

  <!-- WeChat -->
  <div class="card">
    <div class="card-head">
      <h3><span class="im-dot" :style="{ background: imDotColor('wechat', channels.wechat.enable) }"></span> {{ $t('channels.wechat_title') }}</h3>
      <div class="card-head-actions">
        <span v-if="channels.wechat.enable" class="badge badge-green"><span class="badge-dot"></span>{{ $t('common.enabled') }}</span>
        <button
          :class="['toggle', { on: channels.wechat.enable }]"
          @click="channels.wechat.enable = !channels.wechat.enable"
        >
          <span class="thumb"></span>
        </button>
      </div>
    </div>
    <div class="card-body" v-if="channels.wechat.enable">
      <!-- Not logged in: prompt + scan button -->
      <div v-if="loginState.state === 'idle' || loginState.state === 'failed'" class="wechat-login-prompt">
        <p class="text-muted">{{ $t('channels.wechat_login_prompt') }}</p>
        <button class="btn btn-primary" @click="startLogin">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>
          {{ $t('channels.scan_login') }}
        </button>
      </div>

      <!-- Scanning: QR code -->
      <div v-if="loginState.state === 'starting' || loginState.state === 'waiting_scan'" class="wechat-login-prompt">
        <div class="qr-wrapper">
          <img
            v-if="loginState.state === 'waiting_scan'"
            :src="api.wechatLoginQrUrl"
            alt="QR"
            class="qr-img"
          />
          <div v-else class="qr-placeholder">{{ $t('channels.qr_loading') }}</div>
        </div>
        <p class="text-muted">{{ $t('channels.qr_scan_hint') }}</p>
        <button class="btn btn-ghost btn-sm" @click="cancelLogin">{{ $t('common.cancel') }}</button>
      </div>

      <!-- Logged in: status + whitelist -->
      <div v-if="loginState.state === 'completed'">
        <div class="field">
          <span class="badge badge-green"><span class="badge-dot"></span>{{ $t('channels.logged_in') }}</span>
        </div>
        <div class="field mt-4">
          <label class="field-label">{{ $t('channels.allowed_users') }}</label>
          <textarea
            class="input-mono"
            :value="channels.wechat.allowed_users.join('\n')"
            @input="channels.wechat.allowed_users = ($event.target as HTMLTextAreaElement).value.split('\n').filter(Boolean)"
            :placeholder="$t('channels.allowed_users_placeholder')"
          ></textarea>
        </div>
      </div>
    </div>
  </div>

</template>

<style scoped>
.wechat-login-prompt {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 32px 20px;
}
.wechat-login-prompt .btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}
.qr-wrapper {
  width: 200px;
  height: 200px;
  border-radius: 8px;
  overflow: hidden;
  background: #fff;
}
.qr-img {
  width: 100%;
  height: 100%;
  object-fit: contain;
}
.qr-placeholder {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-secondary);
  font-size: 13px;
  background: var(--bg-secondary);
}
</style>
