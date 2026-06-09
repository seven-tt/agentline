import { reactive, ref, onMounted, onUnmounted } from 'vue'
import { api } from '../api'
import type { OverviewData } from '../types'

const overview = reactive<OverviewData>({
  version: '',
  uptime_secs: 0,
  pid: 0,
  agent_backend: '',
  ims: [],
})
const loading = ref(false)

let timer: ReturnType<typeof setInterval> | null = null
let refCount = 0

async function refresh() {
  loading.value = true
  try {
    const data = await api.getOverview()
    Object.assign(overview, data)
  } catch {
    // ignore transient failures
  } finally {
    loading.value = false
  }
}

export function useStatus() {
  onMounted(() => {
    if (refCount === 0) {
      refresh()
      timer = setInterval(refresh, 5000)
    }
    refCount++
  })
  onUnmounted(() => {
    refCount--
    if (refCount === 0 && timer) {
      clearInterval(timer)
      timer = null
    }
  })
  return { overview, loading, refresh }
}
