/**
 * 云书 — 核心事件桥（替代 demo 的 socket.io 层）
 * core（yunshu_core）事件 → store；relayActions → Tauri invoke
 */
import { invoke } from '@tauri-apps/api/core'
import { reactive } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { store, mutations } from '../stores/relay-store'
import { toast } from './use-toast'
import { uuid } from '../lib/relay-format'
import type { Transfer, TransferStatus } from '../lib/relay-types'

/* ---------- 全局拖拽状态（聊天区遮罩用） ---------- */
export const dragState = reactive({ active: false })

/* ---------- 核心 TransferInfo → demo Transfer 模型映射 ---------- */

const STATUS_MAP: Record<string, TransferStatus> = {
  Waiting: 'pending',
  Active: 'uploading',
  Done: 'completed',
  Refused: 'rejected',
  Failed: 'cancelled',
}

function upsertFromInfo(info: any) {
  const isPhone = info.peer === 'phone'
  const toMe = info.direction === 'FromPhone'
  const peerId = info.peer || ''
  const peerName = isPhone ? (store.phoneConnected ? store.phoneName : '手机') : peerNameOf(peerId)
  const t: Transfer = {
    id: info.id,
    from: toMe ? peerId : 'me',
    fromName: toMe ? peerName : store.meName,
    to: toMe ? 'me' : peerId,
    toName: toMe ? store.meName : peerName,
    kind: 'files',
    files: [{ id: info.id, name: info.name, size: info.size }],
    totalSize: info.size || info.transferred || 0,
    status: STATUS_MAP[info.status] ?? 'pending',
    sentBytes: info.transferred || 0,
    createdAt: firstSeen(info.id),
    path: info.path,
  }
  // 保留已有 createdAt（避免每次进度事件刷新时间）
  const prev = store.transfers[info.id]
  if (prev) {
    t.createdAt = prev.createdAt
    t.fromName = prev.from === t.from ? t.fromName : t.fromName
  }
  mutations.upsertTransfer(t)
}

const createdAtMap = new Map<string, number>()
function firstSeen(id: string): number {
  let t = createdAtMap.get(id)
  if (!t) {
    t = Date.now()
    createdAtMap.set(id, t)
  }
  return t
}

function peerNameOf(peerId: string): string {
  if (peerId === 'phone') return store.phoneName
  return store.devices.find((d) => d.id === peerId)?.name || '电脑'
}

/* ---------- core 事件 → store ---------- */

async function apply(ev: any) {
  switch (ev.type) {
    case 'server_started':
      mutations.setServerAddr(ev.addr)
      mutations.setConnection('connected')
      break
    case 'server_error':
      mutations.setServerError(ev.reason)
      mutations.setConnection('disconnected')
      toast.error(ev.reason)
      break
    case 'qr_url':
      mutations.setQrUrl(ev.url)
      break
    case 'pair_pending':
      mutations.setPairPending({ code: ev.code, device: ev.device })
      toast.info(`手机「${ev.device}」发起配对，请在手机上输入配对码`)
      break
    case 'pair_ok':
      mutations.setPairPending(null)
      mutations.setPhoneConnected(ev.device)
      mutations.setSelectedPeer('phone')
      toast.success(`手机「${ev.device}」配对成功`)
      break
    case 'peer_disconnected':
      if (store.phoneConnected) {
        mutations.setPhoneConnected(null)
        toast.info('手机已断开')
      }
      break
    case 'text_from_phone':
      mutations.addMessage({
        id: uuid(), from: 'phone', fromName: store.phoneName, to: 'me',
        kind: 'clipboard', content: ev.text, createdAt: Date.now(),
      })
      if (store.selectedPeer !== 'phone') {
        toast.info(`📱 手机发来了文本（已复制到电脑剪贴板）`)
      }
      break
    case 'toast':
      toast.info(ev.text)
      break
    case 'transfer_update':
      upsertFromInfo(ev)
      break
    case 'ask_save_dir':
      {
        const { open } = await import('@tauri-apps/plugin-dialog')
        const dir = await open({ directory: true, title: '选择保存位置（不选则 2 分钟后用默认目录）' })
        invoke('set_save_dir', { batch: ev.batch, dir })
      }
      break
    case 'pigeon_peers': {
      mutations.setDevices(
        (ev.peers || []).map((p: any) => ({ id: p.id, name: p.name, ip: p.ip, online: p.connected }))
      )
      // 选中设备消失时清空选中
      if (store.selectedPeer && store.selectedPeer !== 'phone' &&
          !store.devices.some((d) => d.id === store.selectedPeer)) {
        mutations.setSelectedPeer('')
      }
      break
    }
    case 'chat_from_pc':
      mutations.addMessage({
        id: uuid(), from: ev.peer_id, fromName: ev.peer_name, to: 'me',
        kind: 'text', content: ev.text, createdAt: Date.now(),
      })
      if (store.selectedPeer !== ev.peer_id) {
        toast.info(`💻 ${ev.peer_name}：${ev.text}`)
      }
      break
    case 'chat_broadcast':
      {
        const dev = ev.peer_id === 'me' ? 'me' : ev.peer_id
        mutations.addMessage({
          id: uuid(), from: dev, fromName: ev.peer_name, to: 'broadcast',
          kind: 'text', content: ev.text, createdAt: Date.now(),
        })
        if (store.selectedPeer !== 'broadcast') toast.info(`📢 ${ev.peer_name}（广播）：${ev.text}`)
      }
      break
    case 'pigeon_trust_request':
      mutations.enqueueTrust({ peer_id: ev.peer_id, name: ev.name, fingerprint: ev.fingerprint })
      break
    case 'pigeon_fingerprint':
      store.fingerprints[ev.peer_id] = ev.fingerprint
      toast.info(`🔒 新设备已固定指纹 ${String(ev.fingerprint).slice(0, 23)}…`)
      break
    case 'pigeon_offer_incoming': {
      // 构造 pending 传输（对方发来的邀请）
      upsertFromInfo({
        id: ev.offer_id, name: ev.name, size: ev.size, peer: ev.peer_id,
        direction: 'FromPhone', status: 'Waiting', transferred: 0,
      })
      const t = store.transfers[ev.offer_id]
      if (t) t.fromName = ev.peer_name
      mutations.enqueueOffer(ev.offer_id)
      toast.info(`「${ev.peer_name}」想发送「${ev.name}」`)
      break
    }
  }
}

/* ---------- relayActions（对外动作，签名与 demo 一致） ---------- */

export const relayActions = {
  sendText(to: string, content: string): boolean {
    const m = {
      id: uuid(), from: 'me', fromName: store.meName,
      to, kind: 'text' as const, content, createdAt: Date.now(),
    }
    if (to === 'phone') {
      if (!store.phoneConnected) { toast.error('手机未配对，请先扫码'); return false }
      invoke('send_text', { text: content })
    } else {
      invoke('pigeon_send_text', { peerId: to, text: content })
    }
    mutations.addMessage(m)
    return true
  },
  sendClipboard(to: string, content: string): boolean {
    return relayActions.sendText(to, content)
  },
  broadcastText(content: string): boolean {
    invoke('broadcast_text', { text: content })
    mutations.addMessage({
      id: uuid(), from: 'me', fromName: store.meName, to: 'broadcast',
      kind: 'text', content, createdAt: Date.now(),
    })
    return true
  },
  /** 文件/文件夹发送：paths 由 Tauri 对话框/拖拽给出 */
  sendPaths(kind: 'phone' | 'pc', peerId: string | null, paths: string[]): boolean {
    if (kind === 'phone' && !store.phoneConnected) {
      toast.error('手机未配对，请先扫码')
      return false
    }
    invoke('send_paths', { kind, peerId, paths })
    toast.success('已发起传输，等待对方确认')
    return true
  },
  respondTransfer(transferId: string, accepted: boolean, peerId?: string) {
    const t = store.transfers[transferId]
    const pid = peerId || (t ? (t.from === 'me' ? t.to : t.from) : '')
    if (t && !accepted) {
      t.status = 'rejected'
    }
    invoke(accepted ? 'pigeon_accept' : 'pigeon_reject', { peerId: pid, offerId: transferId })
  },
  openFolder(path: string) {
    invoke('open_folder', { path })
  },
  disconnectPeer(peerId: string) {
    invoke('pigeon_disconnect', { peerId })
  },
}

/* ---------- 生命周期：挂在应用根部 ---------- */

let started = false

export async function startRelay() {
  if (started) return
  started = true
  const settings = await invoke<any>('get_settings').catch(() => ({}))
  if (settings.device_name) store.meName = settings.device_name

  const snap = await invoke<any>('get_state')
  mutations.setServerAddr(snap.addr)
  mutations.setServerError(snap.error)
  mutations.setQrUrl(snap.qr_url)
  if (snap.fingerprints) store.fingerprints = snap.fingerprints
  mutations.setPairPending(snap.pending)
  if (snap.connected) mutations.setPhoneConnected(snap.connected)
  if (snap.peers) {
    mutations.setDevices(snap.peers.map((p: any) => ({ id: p.id, name: p.name, ip: p.ip, online: p.connected })))
  }
  for (const t of snap.transfers || []) upsertFromInfo(t)

  await listen('core-event', (e: any) => { void apply(e.payload) })
  await listen('tauri://drag-drop', (e: any) => {
    const p = e.payload || {}
    if (p.type === 'enter' || p.type === 'over') {
      if (p.paths?.length) dragState.active = true
    } else if (p.type === 'leave' || p.type === 'drop') {
      dragState.active = false
    }
    if (p.type === 'drop' && p.paths?.length) {
      window.dispatchEvent(new CustomEvent('yunshu-drop', { detail: p.paths as string[] }))
    }
  })
}

/* ---------- 旧导出兼容（下载地址在桌面端为本地路径，不再需要） ---------- */
export function markTransferDownloadedLocal(id: string) {
  mutations.markTransferDownloaded(id)
}
