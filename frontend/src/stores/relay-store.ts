/**
 * 云书 — 全局状态（移植自 demo relay-store，zustand → Vue reactive）
 */
import { reactive } from 'vue'
import type { ChatMessage, ConnectionState, Device, Transfer } from '../lib/relay-types'

export const PANEL_PREF_KEY = 'yunshu.panel.transfer'
export const CHAT_STYLE_KEY = 'yunshu.chat.style'

export type ChatStyle = 'bubble' | 'list'

export interface RelayState {
  meName: string
  meIp: string
  connection: ConnectionState
  devices: Device[]           // 飞鸽电脑设备（手机作为伪设备单独处理）
  selectedPeer: string        // 'phone' | 'broadcast' 或 deviceId
  phoneConnected: boolean     // 手机配对状态
  phoneName: string
  messages: ChatMessage[]
  transfers: Record<string, Transfer>
  offerQueue: string[]
  currentOfferId: string | null
  serverAddr: string
  serverError: string | null
  qrUrl: string | null
  fingerprints: Record<string, string>
  trustRequests: Array<{ peer_id: string; name: string; fingerprint: string }>
  pairPending: { code: string; device: string } | null
  /** 右侧传输面板（lg+ 常驻第三栏）是否展开；开闭偏好持久化 */
  transferPanelOpen: boolean
  /** 聊天消息显示格式：bubble 气泡 / list 列表（IRC 式）；偏好持久化 */
  chatStyle: ChatStyle
  /** 防打扰：仅已信任设备可连接，广播静音 */
  dnd: boolean
}

export const store = reactive<RelayState>({
  meName: '我的设备',
  meIp: '',
  connection: 'connecting',
  devices: [],
  selectedPeer: 'broadcast',
  phoneConnected: false,
  phoneName: '手机',
  messages: [],
  transfers: {},
  offerQueue: [],
  currentOfferId: null,
  serverAddr: '',
  serverError: null,
  qrUrl: null,
  fingerprints: {},
  trustRequests: [],
  pairPending: null,
  transferPanelOpen: true,
  chatStyle: 'bubble',
  dnd: false,
})

export const mutations = {
  setConnection(c: ConnectionState) { store.connection = c },
  setServerAddr(a: string) {
    store.serverAddr = a
    const ip = a?.split(':')[0]
    if (ip) store.meIp = ip
  },
  setServerError(e: string) { store.serverError = e },
  setQrUrl(u: string) { store.qrUrl = u },
  setPairPending(p: { code: string; device: string } | null) { store.pairPending = p },
  setPhoneConnected(name: string | null) {
    store.phoneConnected = !!name
    if (name) store.phoneName = name
  },
  setDevices(devices: Device[]) { store.devices = devices },
  setSelectedPeer(peer: string) { store.selectedPeer = peer },
  addMessage(m: ChatMessage) {
    // 同 id 去重（避免本地插入与服务端回显重复）
    if (store.messages.some((x) => x.id === m.id)) return
    store.messages.push(m)
  },
  upsertTransfer(t: Transfer) { store.transfers[t.id] = t },
  updateTransferProgress(id: string, sentBytes: number, totalSize?: number) {
    const t = store.transfers[id]
    if (!t) return
    t.sentBytes = sentBytes
    if (totalSize && totalSize > 0) t.totalSize = totalSize
  },
  setTransferStatus(id: string, status: Transfer['status']) {
    const t = store.transfers[id]
    if (t) t.status = status
  },
  markTransferDownloaded(id: string) {
    const t = store.transfers[id]
    if (t && t.status !== 'completed') t.status = 'completed'
  },
  removeTransfer(id: string) {
    delete store.transfers[id]
  },
  enqueueTrust(req: { peer_id: string; name: string; fingerprint: string }) {
    if (store.trustRequests.some((r) => r.peer_id === req.peer_id)) return
    store.trustRequests.push(req)
  },
  dequeueTrust(peerId: string) {
    store.trustRequests = store.trustRequests.filter((r) => r.peer_id !== peerId)
  },
  enqueueOffer(transferId: string) {
    if (!store.offerQueue.includes(transferId)) store.offerQueue.push(transferId)
    store.currentOfferId = store.currentOfferId ?? store.offerQueue[0] ?? null
  },
  dequeueOffer() {
    const cur = store.currentOfferId
    store.offerQueue = store.offerQueue.filter((x) => x !== cur)
    store.currentOfferId = store.offerQueue[0] ?? null
  },
  setTransferPanelOpen(open: boolean) { store.transferPanelOpen = open },
  setChatStyle(style: ChatStyle) { store.chatStyle = style },
  setDnd(enabled: boolean) { store.dnd = enabled },
}

/* --------------------------- 第三栏面板偏好 --------------------------- */

/** 切换传输面板开闭（含 localStorage 持久化） */
export function toggleTransferPanelPersist() {
  const next = !store.transferPanelOpen
  mutations.setTransferPanelOpen(next)
  try {
    localStorage.setItem(PANEL_PREF_KEY, next ? '1' : '0')
  } catch { /* 忽略持久化失败 */ }
}

/** 展开传输面板（含 localStorage 持久化） */
export function openTransferPanelPersist() {
  if (store.transferPanelOpen) return
  toggleTransferPanelPersist()
}

/** 启动时恢复第三栏开闭偏好 */
export function restoreTransferPanelPref() {
  try {
    const saved = localStorage.getItem(PANEL_PREF_KEY)
    if (saved !== null) mutations.setTransferPanelOpen(saved === '1')
  } catch { /* 忽略 */ }
}

/* --------------------------- 聊天格式偏好 --------------------------- */

/** 切换气泡/列表显示格式（含 localStorage 持久化） */
export function setChatStylePersist(style: ChatStyle) {
  mutations.setChatStyle(style)
  try {
    localStorage.setItem(CHAT_STYLE_KEY, style)
  } catch { /* 忽略持久化失败 */ }
}

/** 启动时恢复聊天格式偏好 */
export function restoreChatStylePref() {
  try {
    const saved = localStorage.getItem(CHAT_STYLE_KEY)
    if (saved === 'bubble' || saved === 'list') mutations.setChatStyle(saved)
  } catch { /* 忽略 */ }
}

/* ------------------------------ 派生选择器 ------------------------------ */

/** 消息是否与选中 peer 相关 */
function messageInvolves(m: ChatMessage, peer: string): boolean {
  if (peer === 'broadcast') return m.to === 'broadcast' || m.kind === 'system'
  if (peer === 'phone') return m.from === 'phone' || m.to === 'phone'
  return (m.from === 'me' && m.to === peer) || (m.from === peer && m.to === 'me')
}

export function filterEntriesForPeer(peer: string) {
  const entries = []
  for (const m of store.messages) {
    if (messageInvolves(m, peer)) {
      entries.push({ type: 'message' as const, key: `m-${m.id}`, message: m })
    }
  }
  for (const t of Object.values(store.transfers)) {
    if (peer === 'phone') {
      if (t.from === 'phone' || t.to === 'phone') {
        entries.push({ type: 'transfer' as const, key: `t-${t.id}`, transfer: t })
      }
    } else if (peer !== 'broadcast') {
      const involved = (t.from === 'me' && t.to === peer) || (t.from === peer && t.to === 'me')
      if (involved) entries.push({ type: 'transfer' as const, key: `t-${t.id}`, transfer: t })
    }
  }
  entries.sort((a, b) => {
    const ta = a.type === 'message' ? a.message.createdAt : a.transfer.createdAt
    const tb = b.type === 'message' ? b.message.createdAt : b.transfer.createdAt
    return ta - tb
  })
  return entries
}

export function deviceHasActiveTransfer(deviceId: string): boolean {
  return Object.values(store.transfers).some(
    (t) =>
      ((t.from === deviceId && t.to === 'me') || (t.from === 'me' && t.to === deviceId)) &&
      (t.status === 'pending' || t.status === 'uploading')
  )
}

export function splitTransfers(): { active: Transfer[]; finished: Transfer[] } {
  const active: Transfer[] = []
  const finished: Transfer[] = []
  for (const t of Object.values(store.transfers)) {
    if (t.status === 'pending' || t.status === 'uploading' || t.status === 'paused') active.push(t)
    else finished.push(t)
  }
  active.sort((a, b) => b.createdAt - a.createdAt)
  finished.sort((a, b) => b.createdAt - a.createdAt)
  return { active, finished }
}
