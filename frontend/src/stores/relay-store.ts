/**
 * 云书 — 全局状态（移植自 demo relay-store，zustand → Vue reactive）
 */
import { reactive } from 'vue'
import type { ChatMessage, ConnectionState, Device, Presence, Transfer } from '../lib/relay-types'

export const PANEL_PREF_KEY = 'yunshu.panel.transfer'
export const CHAT_STYLE_KEY = 'yunshu.chat.style'

export type ChatStyle = 'bubble' | 'list'

export interface RelayState {
  meName: string
  meIp: string
  connection: ConnectionState
  devices: Device[]           // 飞鸽电脑设备（手机作为伪设备单独处理）
  selectedPeer: string        // 'self'（本机识别首页）| 'phone' | 'broadcast' 或 deviceId
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
  /** 隐身模式：不向局域网广播本机（头像状态灯与设置弹窗用） */
  stealth: boolean
  /** 未读消息数：key 为 peer id（'broadcast' | 'phone' | 设备 id），切入会话即清零 */
  unread: Record<string, number>
  /** 远程控制：本机临时密码（每次启动重新生成） */
  remoteTempPassword: string | null
  /** 远程控制：本机公开机器 ID */
  remoteMachineId: string | null
  /** 远程控制：本机正在被控（受控端醒目横幅） */
  remoteControlled: { peer_id: string; name: string } | null
  /** 远程控制：本机正在观看对方（供手机「只看不控」扫码接入） */
  remoteViewing: { name: string; view_token: string; port: number; ip: string } | null
  /** 远程控制：发起连接的密码输入弹窗 */
  controlDialog: { machine_id: string; name: string } | null
  /** 远程控制：待处理的反向协助请求队列 */
  assistRequests: Array<{ peer_id: string; name: string }>
  /** 微信通道：最近收到的消息（文本与已落盘媒体，环形 50 条） */
  weixinInbound: Array<{
    at_ms: number; from: string; kind: string; text: string
    file_name?: string; size?: number; saved_path?: string; error?: string
  }>
}

export const store = reactive<RelayState>({
  meName: '我的设备',
  meIp: '',
  connection: 'connecting',
  devices: [],
  selectedPeer: 'self',
  phoneConnected: false,
  phoneName: '手机',
  messages: [],
  transfers: {},
  offerQueue: [],
  currentOfferId: null,
  serverAddr: '',
  serverError: null,
  qrUrl: null,
  qrAlts: [] as string[],
  fingerprints: {},
  trustRequests: [],
  pairPending: null,
  transferPanelOpen: true,
  chatStyle: 'bubble',
  dnd: false,
  stealth: false,
  unread: {},
  remoteTempPassword: null,
  remoteMachineId: null,
  remoteControlled: null,
  remoteViewing: null,
  controlDialog: null,
  assistRequests: [],
  weixinInbound: [],
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
  setQrAlts(list: string[]) { store.qrAlts = list },
  setPairPending(p: { code: string; device: string } | null) { store.pairPending = p },
  setPhoneConnected(name: string | null) {
    store.phoneConnected = !!name
    if (name) store.phoneName = name
  },
  setDevices(devices: Device[]) { store.devices = devices },
  setSelectedPeer(peer: string) {
    store.selectedPeer = peer
    // 切入会话即视为已读
    delete store.unread[peer]
  },
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
  addWeixinInbound(m: RelayState['weixinInbound'][number]) {
    store.weixinInbound.push(m)
    if (store.weixinInbound.length > 50) store.weixinInbound.shift()
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
  bumpUnread(peer: string) {
    if (peer === 'me') return
    store.unread[peer] = (store.unread[peer] || 0) + 1
  },
  clearUnread(peer: string) {
    delete store.unread[peer]
  },
  /* ---- 远程控制 ---- */
  setRemoteTempPassword(pw: string | null) { store.remoteTempPassword = pw },
  setRemoteMachineId(id: string | null) { store.remoteMachineId = id },
  setRemoteControlled(c: { peer_id: string; name: string } | null) { store.remoteControlled = c },
  setRemoteViewing(v: { name: string; view_token: string; port: number; ip: string } | null) { store.remoteViewing = v },
  openControlDialog(machineId: string, name: string) { store.controlDialog = { machine_id: machineId, name } },
  closeControlDialog() { store.controlDialog = null },
  enqueueAssist(req: { peer_id: string; name: string }) {
    if (store.assistRequests.some((r) => r.peer_id === req.peer_id)) return
    store.assistRequests.push(req)
  },
  dequeueAssist(peerId: string) {
    store.assistRequests = store.assistRequests.filter((r) => r.peer_id !== peerId)
  },
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

/** 某 peer 的未读消息数 */
export function unreadFor(peer: string): number {
  return store.unread[peer] || 0
}

export function deviceHasActiveTransfer(deviceId: string): boolean {
  return Object.values(store.transfers).some(
    (t) =>
      ((t.from === deviceId && t.to === 'me') || (t.from === 'me' && t.to === deviceId)) &&
      (t.status === 'pending' || t.status === 'uploading')
  )
}

/** 本机状态（隐身 > 免打扰 > 在线，同时开启时隐身优先；侧栏/首页状态灯共用） */
export function myPresence(): Presence {
  return store.stealth ? 'stealth' : store.dnd ? 'dnd' : 'online'
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
