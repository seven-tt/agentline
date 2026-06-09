<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import { api } from '../api'

interface LogLine {
  time: string
  level: string
  source: string
  msg: string
}

const rawText = ref('')
const activeFilters = ref<Set<string>>(new Set(['info', 'warn', 'error', 'debug']))
const logEl = ref<HTMLElement | null>(null)
let timer: ReturnType<typeof setInterval> | null = null

const levels = ['info', 'warn', 'error', 'debug'] as const

function toggleFilter(level: string) {
  if (activeFilters.value.has(level)) {
    activeFilters.value.delete(level)
  } else {
    activeFilters.value.add(level)
  }
}

function parseLogs(text: string): LogLine[] {
  return text
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const m = line.match(/^(\d{4}-\d{2}-\d{2}T[\d:.]+)\s+(\w+)\s+(\S+):\s*(.*)$/)
      if (m) {
        return { time: m[1].split('T')[1]?.slice(0, 8) ?? '', level: m[2].toLowerCase(), source: m[3], msg: m[4] }
      }
      const m2 = line.match(/^\s*(\d{4}-\d{2}-\d{2}T[\d:.Z+]+)\s+(\w+)\s+(.*)$/)
      if (m2) {
        return { time: m2[1].split('T')[1]?.slice(0, 8) ?? '', level: m2[2].toLowerCase(), source: '', msg: m2[3] }
      }
      return { time: '', level: 'info', source: '', msg: line }
    })
}

const logs = computed(() => parseLogs(rawText.value))
const filtered = computed(() =>
  logs.value.filter((l) => activeFilters.value.has(l.level)),
)

async function fetchLogs() {
  try {
    rawText.value = await api.getLogs()
    await nextTick()
    if (logEl.value) {
      logEl.value.scrollTop = logEl.value.scrollHeight
    }
  } catch { /* ignore */ }
}

onMounted(() => {
  fetchLogs()
  timer = setInterval(fetchLogs, 3000)
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <div class="log-filters">
    <button
      v-for="level in levels"
      :key="level"
      :class="['log-filter', { active: activeFilters.has(level) }]"
      @click="toggleFilter(level)"
    >
      {{ level.toUpperCase() }}
    </button>
  </div>

  <div class="log-viewer" ref="logEl">
    <div v-if="filtered.length === 0" class="empty-state" style="padding: 24px">
      {{ $t('logs.no_logs') }}
    </div>
    <div v-for="(l, i) in filtered" :key="i" class="log-line">
      <span class="log-time">{{ l.time }}</span>
      <span :class="['log-level', l.level]">{{ l.level }}</span>
      <span v-if="l.source" class="log-source">{{ l.source }}</span>
      <span class="log-msg">{{ l.msg }}</span>
    </div>
  </div>
</template>
