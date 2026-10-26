/**
 * 云书 — 格式化与展示工具（移植自 demo relay-format，图标用 @lucide/vue）
 */
import {
  File, FileText, FileImage, FileVideo, FileAudio, FileArchive, FileType, FileCode2, Folder,
} from '@lucide/vue'
import type { Component } from 'vue'
import type { FileCategory } from './relay-types'

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '0 B'
  if (bytes < 1024) return `${Math.round(bytes)} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unitIndex = -1
  do {
    value /= 1024
    unitIndex++
  } while (value >= 1024 && unitIndex < units.length - 1)
  const digits = value >= 100 ? 0 : value >= 10 ? 1 : 2
  return `${value.toFixed(digits)} ${units[unitIndex]}`
}

export function formatSpeed(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return '—'
  const mb = bytesPerSecond / (1024 * 1024)
  if (mb >= 1) return `${mb.toFixed(mb >= 10 ? 0 : 1)} MB/s`
  return `${Math.max(1, Math.round(bytesPerSecond / 1024))} KB/s`
}

const TWO_DAYS_MS = 2 * 24 * 60 * 60 * 1000
function pad2(n: number): string {
  return n.toString().padStart(2, '0')
}

/** HH:mm；昨天显示「昨天 HH:mm」；更早显示「M月D日 HH:mm」 */
export function formatTime(timestamp: number): string {
  const date = new Date(timestamp)
  const now = new Date()
  const timePart = `${pad2(date.getHours())}:${pad2(date.getMinutes())}`
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  if (sameDay) return timePart
  const yesterday = new Date(now.getTime() - TWO_DAYS_MS / 2)
  yesterday.setDate(now.getDate() - 1)
  if (
    date.getFullYear() === yesterday.getFullYear() &&
    date.getMonth() === yesterday.getMonth() &&
    date.getDate() === yesterday.getDate()
  ) {
    return `昨天 ${timePart}`
  }
  const sameYear = date.getFullYear() === now.getFullYear()
  const datePart = sameYear
    ? `${date.getMonth() + 1}月${date.getDate()}日`
    : `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日`
  return `${datePart} ${timePart}`
}

const EXT_MAP: Record<string, FileCategory> = {
  png: 'image', jpg: 'image', jpeg: 'image', gif: 'image', webp: 'image', svg: 'image', bmp: 'image', ico: 'image',
  mp4: 'video', mkv: 'video', mov: 'video', avi: 'video', webm: 'video',
  mp3: 'audio', wav: 'audio', flac: 'audio', aac: 'audio',
  zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive', gz: 'archive',
  pdf: 'pdf',
  doc: 'doc', docx: 'doc', xls: 'doc', xlsx: 'doc', ppt: 'doc', pptx: 'doc', csv: 'doc', txt: 'doc', md: 'doc',
  ts: 'code', tsx: 'code', js: 'code', json: 'code', html: 'code', css: 'code', py: 'code', go: 'code', rs: 'code',
}

export function fileCategory(name: string): FileCategory {
  const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : ''
  return EXT_MAP[ext] ?? 'other'
}

interface IconStyle {
  icon: Component
  className: string
}

const CATEGORY_STYLE: Record<FileCategory, IconStyle> = {
  image: { icon: FileImage, className: 'text-sky-500 bg-sky-500/10' },
  video: { icon: FileVideo, className: 'text-rose-500 bg-rose-500/10' },
  audio: { icon: FileAudio, className: 'text-violet-500 bg-violet-500/10' },
  archive: { icon: FileArchive, className: 'text-amber-500 bg-amber-500/10' },
  pdf: { icon: FileType, className: 'text-red-500 bg-red-500/10' },
  doc: { icon: FileText, className: 'text-emerald-600 bg-emerald-500/10' },
  code: { icon: FileCode2, className: 'text-teal-500 bg-teal-500/10' },
  other: { icon: File, className: 'text-muted-foreground bg-muted' },
}

export function fileIconStyle(name: string): IconStyle {
  return CATEGORY_STYLE[fileCategory(name)]
}

export function folderIconStyle(): IconStyle {
  return { icon: Folder, className: 'text-amber-500 bg-amber-500/10' }
}

/* ------------------------------ 渐变头像 ------------------------------ */

const GRADIENTS = [
  'from-emerald-400 to-teal-600',
  'from-orange-400 to-rose-500',
  'from-violet-400 to-purple-600',
  'from-amber-400 to-orange-600',
  'from-rose-400 to-pink-600',
  'from-cyan-400 to-sky-600',
  'from-lime-400 to-green-600',
  'from-fuchsia-400 to-purple-600',
]

export function gradientForName(name: string): string {
  let hash = 0
  for (let i = 0; i < name.length; i++) {
    hash = (hash * 31 + name.charCodeAt(i)) | 0
  }
  return GRADIENTS[Math.abs(hash) % GRADIENTS.length]
}

export function avatarInitial(name: string): string {
  const trimmed = (name ?? '').trim()
  if (!trimmed) return '?'
  const first = trimmed[0]
  return /[\u4e00-\u9fa5]/.test(first) ? first : first.toUpperCase()
}

let uuidCounter = 0
export function uuid(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `id-${Date.now().toString(36)}-${(uuidCounter++).toString(36)}`
}
