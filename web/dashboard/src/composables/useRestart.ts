import { ref } from 'vue'

const STORAGE_KEY = 'agentline_restart'

function loadState(): { needsRestart: boolean; lastModifiedAt: number } {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) return JSON.parse(raw)
  } catch { /* ignore */ }
  return { needsRestart: false, lastModifiedAt: 0 }
}

function saveState() {
  localStorage.setItem(STORAGE_KEY, JSON.stringify({
    needsRestart: needsRestart.value,
    lastModifiedAt: lastModifiedAt.value,
  }))
}

const stored = loadState()
const needsRestart = ref(stored.needsRestart)
const lastModifiedAt = ref(stored.lastModifiedAt)
const showDialog = ref(false)

function markDirty() {
  needsRestart.value = true
  lastModifiedAt.value = Date.now()
  saveState()
}

function triggerDialog() {
  if (needsRestart.value) {
    showDialog.value = true
  }
}

function dismiss() {
  showDialog.value = false
}

function clearRestart() {
  needsRestart.value = false
  lastModifiedAt.value = 0
  showDialog.value = false
  localStorage.removeItem(STORAGE_KEY)
}

export function useRestart() {
  return { needsRestart, lastModifiedAt, showDialog, markDirty, triggerDialog, dismiss, clearRestart }
}
