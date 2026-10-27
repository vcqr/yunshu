/**
 * 云书 — 轻量 toast（替代 demo 的 sonner）
 */
import { reactive } from 'vue'

interface ToastItem { id: number; kind: 'success' | 'error' | 'info' | 'warning'; text: string }

export const toastState = reactive<{ items: ToastItem[] }>({ items: [] })
let seq = 0

function push(kind: ToastItem['kind'], text: string) {
  const id = ++seq
  toastState.items.push({ id, kind, text })
  setTimeout(() => {
    const i = toastState.items.findIndex((t) => t.id === id)
    if (i >= 0) toastState.items.splice(i, 1)
  }, kind === 'warning' ? 8000 : 4200)
}

export const toast = {
  success: (text: string) => push('success', text),
  error: (text: string) => push('error', text),
  info: (text: string) => push('info', text),
  warning: (text: string) => push('warning', text),
}
