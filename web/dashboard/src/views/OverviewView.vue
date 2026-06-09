<script setup lang="ts">
import { inject, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { OverviewData } from '../types'

const { t } = useI18n()
const overview = inject<OverviewData>('overview')!

const imMeta = computed<Record<string, { label: string; color: string }>>(() => ({
  wechat: { label: 'WeChat', color: '#07c160' },
  dingtalk: { label: 'DingTalk', color: '#0089ff' },
  feishu: { label: t('channels.feishu_label'), color: '#3370ff' },
  telegram: { label: 'Telegram', color: '#0088cc' },
}))

const enabledIms = computed(() => overview.ims.filter((im) => im.enabled))
const totalSessions = computed(() =>
  overview.ims.reduce((sum, im) => sum + im.sessions.length, 0),
)

function formatUptime(secs: number): string {
  if (!secs) return '-'
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  return h > 0 ? `${h}h ${m}m` : `${m}m`
}
</script>

<template>
  <div class="grid grid-3 mb-4">
    <div class="card">
      <div class="stat">
        <div class="stat-value" style="color: var(--green)">
          {{ enabledIms.length }}
        </div>
        <div class="stat-label">{{ $t('overview.active_ims') }}</div>
      </div>
    </div>
    <div class="card">
      <div class="stat">
        <div class="stat-value">{{ totalSessions }}</div>
        <div class="stat-label">{{ $t('overview.active_sessions') }}</div>
      </div>
    </div>
    <div class="card">
      <div class="stat">
        <div class="stat-value">{{ formatUptime(overview.uptime_secs) }}</div>
        <div class="stat-label">{{ $t('overview.uptime') }}</div>
      </div>
    </div>
  </div>

  <div v-if="enabledIms.length === 0" class="card">
    <div class="empty-state">{{ $t('overview.no_ims') }}</div>
  </div>

  <div class="card" v-for="im in enabledIms" :key="im.id">
    <div class="im-card-header">
      <span class="im-dot" :style="{ background: im.healthy ? 'var(--green)' : 'var(--red)' }"></span>
      <span class="im-name">{{ imMeta[im.id]?.label ?? im.id }}</span>
      <span
        class="badge"
        :class="im.healthy ? 'badge-green' : 'badge-red'"
      >
        <span class="badge-dot"></span>
        {{ im.healthy ? $t('overview.running') : $t('overview.error') }}
      </span>
    </div>
    <div
      style="
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
        gap: 12px;
        padding: 14px 20px;
        background: var(--bg-tertiary);
      "
    >
      <div>
        <div class="text-xs text-muted" style="margin-bottom: 2px">{{ $t('overview.agent_backend') }}</div>
        <div class="text-sm text-mono">{{ overview.agent_backend }}</div>
      </div>
      <div>
        <div class="text-xs text-muted" style="margin-bottom: 2px">PID</div>
        <div class="text-sm text-mono">{{ overview.pid }}</div>
      </div>
      <div>
        <div class="text-xs text-muted" style="margin-bottom: 2px">{{ $t('overview.sessions') }}</div>
        <div class="text-sm">{{ im.sessions.length }}</div>
      </div>
      <div>
        <div class="text-xs text-muted" style="margin-bottom: 2px">{{ $t('overview.status') }}</div>
        <div class="text-sm" :style="{ color: im.healthy ? 'var(--green)' : 'var(--red)' }">
          {{ im.healthy ? 'healthy' : 'unhealthy' }}
        </div>
      </div>
    </div>
    <div v-if="im.sessions.length > 0" style="padding: 0 20px 16px">
      <table style="width: 100%; font-size: 12px; border-collapse: collapse; margin-top: 12px">
        <thead>
          <tr style="color: var(--text-tertiary); text-align: left">
            <th style="padding: 6px 8px; font-weight: 500">{{ $t('overview.table_id') }}</th>
            <th style="padding: 6px 8px; font-weight: 500">{{ $t('overview.table_user') }}</th>
            <th style="padding: 6px 8px; font-weight: 500">{{ $t('overview.table_status') }}</th>
            <th style="padding: 6px 8px; font-weight: 500">{{ $t('overview.table_cwd') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="s in im.sessions"
            :key="s.id"
            style="border-top: 1px solid var(--border-muted)"
          >
            <td style="padding: 6px 8px; font-family: var(--font-mono)">{{ s.id.slice(0, 8) }}</td>
            <td style="padding: 6px 8px">{{ s.user }}</td>
            <td style="padding: 6px 8px">
              <span
                class="badge badge-sm"
                :class="s.active ? 'badge-green' : 'badge-muted'"
              >
                {{ s.active ? 'active' : 'idle' }}
              </span>
            </td>
            <td style="padding: 6px 8px; font-family: var(--font-mono); color: var(--text-secondary)">
              {{ s.cwd }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
