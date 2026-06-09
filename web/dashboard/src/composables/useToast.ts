import { ref } from 'vue'
import type { ToastItem } from '../types'

const toasts = ref<ToastItem[]>([])
let nextId = 0

function addToast(type: ToastItem['type'], message: string, duration = 4000) {
  const id = nextId++
  toasts.value.push({ id, type, message })
  setTimeout(() => {
    toasts.value = toasts.value.filter((t) => t.id !== id)
  }, duration)
}

export function useToast() {
  return { toasts, addToast }
}
