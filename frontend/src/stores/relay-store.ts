/**
 * 云书 — 全局状态（移植自 demo relay-store，zustand → Vue reactive）
 */
import { reactive } from 'vue'
import type { ChatMessage, ConnectionState, Device, Transfer } from '../lib/relay-types'

export interface RelayState {
  meName: string
  meIp: string
  connection: ConnectionState
  devices: Device[]           // 飞鸽电脑设备（手机作为伪设备单独处理）
  selectedPeer: string        // 'phone' 或 deviceId
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
  chatHistories: Record<string, ChatMessage[]>  // 已废弃占位：时间线从 messages 派生
}

export const store = reactive<RelayState>({
  meName: '我的设备',
  meIp: '',
  connection: 'connecting',
  devices: [],
  selectedPeer: '',
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
  chatHistories: {},
})

export const mutations = {
  setConnection(c: ConnectionState) { store.connection = c },
  setServerAddr(a: string) { store.serverAddr = a },
  setServerError(e: string) { store.serverError = e },
  setQrUrl(u: string) { store.qrUrl = u },
  setPairPending(p: { code: string; device: string } | null) { store.pairPending = p },
  setPhoneConnected(name: string | null) {
    store.phoneConnected = !!name
    if (name) store.phoneName = name
  },
  setDevices(devices: Device[]) { store.devices = devices },
  setSelectedPeer(peer: string) { store.selectedPeer = peer },
  addMessage(m: ChatMessage) { store.messages.push(m) },
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
  enqueueTrust(req: { peer_id: string; name: string; fingerprint: string }) {
    if (store.trustRequests.some((r) => r.peer_id === req.peer_id)) return
    store.trustRequests.push(req)
  },
  dequeueTrust(peerId: string) {
    state.trustRequests = store.trustRequests.filter((r) => r.peer_id !== peerId)
  },
  enqueueOffer(transferId: string) {
    if (!store.offerQueue.includes(transferId)) store.offerQueue.push(transferId)
    store.currentOfferId = store.currentOfferId ?? store.offerQueue[0] ?? null
  },
  dequeueOffer() {
    store.offerQueue.shift()
    store.currentOfferId = store.offerQueue[0] ?? null
  },
}

/* ------------------------------ 派生选择器 ------------------------------ */

/** 消息是否与选中 peer 相关 */
function messageInvolves(m: ChatMessage, peer: string): boolean {
  if (peer === 'broadcast') return m.to === 'broadcast'
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
    } else {
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
    if (t.status === 'pending' || t.status === 'uploading') active.push(t)
    else finished.push(t)
  }
  active.sort((a, b) => b.createdAt - a.createdAt)
  finished.sort((a, b) => b.createdAt - a.createdAt)
  return { active, finished }
}
