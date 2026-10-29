/**
 * Yunshu — 核心事件桥（替代 demo 的 socket.io 层）
 * core（yunshu_core）事件 → store；relayActions → Tauri invoke
 */
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow, UserAttentionType } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { reactive } from 'vue'
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
  Cancelled: 'cancelled',
  Paused: 'paused',
}

function upsertFromInfo(info: any) {
  // 文本互发以聊天消息呈现（core 另发 TextFromPhone/聊天事件），不再生成传输卡
  if (info.kind === 'Text') return
  const isPhone = info.peer === 'phone'
  const toMe = info.direction === 'FromPhone'
  const peerId = info.peer || ''
  const peerName = isPhone ? (store.phoneConnected ? store.phoneName : '手机') : peerNameOf(peerId)
  const files: TransferFileMeta[] = (info.files?.length
    ? info.files
    : [{ name: info.name, size: info.size }]
  ).map((f: { name: string; size: number }, i: number) => ({ id: `${info.id}-${i}`, name: f.name, size: f.size }));
  const t: Transfer = {
    id: info.id,
    from: toMe ? peerId : 'me',
    fromName: toMe ? peerName : store.meName,
    to: toMe ? 'me' : peerId,
    toName: toMe ? store.meName : peerName,
    kind: files.some((f) => f.name.includes('/')) ? 'folder' : 'files',
    files,
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
    // 局部更新保护：取消回执等可能缺名称/进度，不覆盖已有信息
    if (!info.name && prev.name) {
      t.name = prev.name
      t.files = prev.files
      t.totalSize = prev.totalSize
    }
    if (t.sentBytes === 0 && prev.sentBytes > 0 && (t.status === 'cancelled' || t.status === 'rejected')) {
      t.sentBytes = prev.sentBytes
    }
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

export function peerNameOf(peerId: string): string {
  if (peerId === 'phone') return store.phoneConnected ? store.phoneName : '手机'
  return store.devices.find((d) => d.id === peerId)?.name || '电脑'
}

/* ---------- 收信感知：未读计数 + 后台时任务栏闪烁 ---------- */

/**
 * 窗口在后台时闪烁任务栏图标（Windows 惯例），窗口获得焦点后系统自动停止。
 * 角标/toast 都在窗口内，最小化或被遮挡时不可见，需要这个窗口级提示补位。
 */
function flashTaskbar() {
  if (document.hasFocus()) return
  try {
    void getCurrentWindow().requestUserAttention(UserAttentionType.Critical)
  } catch { /* 非 Tauri 环境（浏览器预览） */ }
}

/* ---------- 飞鸽设备上下线 → 广播会话系统消息 ---------- */

let peersInitialized = false
function systemMessage(content: string) {
  mutations.addMessage({
    id: uuid(), from: 'server', fromName: '系统', to: 'broadcast',
    kind: 'system', content, createdAt: Date.now(),
  })
}

function applyPeers(peers: any[]) {
  const devices = (peers || []).map((p: any) => ({
    id: p.id, name: p.name, ip: p.ip, online: p.connected,
    machineId: p.machine_id || '', manual: !!p.manual, addr: p.addr || '',
  }))
  if (peersInitialized) {
    for (const d of devices) {
      if (!store.devices.some((x) => x.id === d.id) && d.online) systemMessage(`${d.name} 上线了`)
    }
    for (const d of store.devices) {
      // 手动主机的离线条目常驻列表，只在真在线的设备消失时提示
      if (!devices.some((x: any) => x.id === d.id) && d.online) systemMessage(`${d.name} 离开了`)
    }
  }
  peersInitialized = true
  // 手动主机连上后 id 从离线占位（manual:addr）变为真实会话 id：选中状态跟随迁移
  let selected = store.selectedPeer
  if (selected && selected.startsWith('manual:')) {
    const live = devices.find((d) => d.addr === selected!.slice(7) && d.id !== selected)
    if (live) {
      selected = live.id
      mutations.setSelectedPeer(live.id)
    }
  }
  mutations.setDevices(devices)
  // 选中设备消失时回到本机识别首页（'self'/'phone'/'weixin'/'broadcast' 为固定视图）
  if (selected && selected !== 'self' && selected !== 'phone' && selected !== 'weixin' && selected !== 'broadcast' &&
      !devices.some((d) => d.id === selected)) {
    mutations.setSelectedPeer('self')
  }
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
    case 'qr_alternates':
      mutations.setQrAlts(ev.urls || [])
      break
    case 'pair_pending':
      mutations.setPairPending({ code: ev.code, device: ev.device })
      mutations.setSelectedPeer('phone')
      toast.info(`手机「${ev.device}」发起配对，请在手机上输入配对码`)
      break
    case 'pair_ok':
      mutations.setPairPending(null)
      mutations.setPhoneConnected(ev.device)
      mutations.setSelectedPeer('phone')
      toast.success(`手机「${ev.device}」配对成功`)
      break
    case 'pair_failed':
      toast.error(ev.reason || '配对失败')
      break
    case 'peer_disconnected':
      if (store.phoneConnected) {
        mutations.setPhoneConnected(null)
        toast.info('手机已断开')
      }
      break
    case 'text_from_phone': {
      // 手机发来的文本按普通消息展示，无剪贴板副作用
      const name = store.phoneConnected ? store.phoneName : '手机'
      mutations.addMessage({
        id: uuid(), from: 'phone', fromName: name, to: 'me',
        kind: 'text', content: ev.text, createdAt: Date.now(),
      })
      if (store.selectedPeer !== 'phone') {
        mutations.bumpUnread('phone')
        toast.info(`${name}：${ev.text}`)
        flashTaskbar()
      }
      break
    }
    case 'weixin_inbound': {
      // 微信通道：消息进「微信」会话；不在会话时才弹轻提示与未读
      const m = ev.msg
      mutations.addWeixinInbound(m)
      mutations.setWeixinLoggedIn(true)
      if (store.selectedPeer !== 'weixin') {
        mutations.bumpUnread('weixin')
        if (m.saved_path) {
          toast.info(`微信收到${m.kind === 'image' ? '图片' : m.kind === 'video' ? '视频' : '文件'}：${m.saved_path}`)
        } else if (m.text) {
          toast.info(`微信：${m.text}`)
        }
      }
      break
    }
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
    case 'pigeon_peers':
      applyPeers(ev.peers || [])
      break
    case 'chat_from_pc':
      mutations.addMessage({
        id: uuid(), from: ev.peer_id, fromName: ev.peer_name, to: 'me',
        kind: 'text', content: ev.text, createdAt: Date.now(),
      })
      if (store.selectedPeer !== ev.peer_id) {
        mutations.bumpUnread(ev.peer_id)
        toast.info(`${ev.peer_name}：${ev.text}`)
        flashTaskbar()
      }
      break
    case 'chat_broadcast':
      // 本机广播由 core 回显（peer_id='me'），直接采用即可
      mutations.addMessage({
        id: uuid(), from: ev.peer_id, fromName: ev.peer_id === 'me' ? store.meName : ev.peer_name,
        to: 'broadcast', kind: 'text', content: ev.text, createdAt: Date.now(),
      })
      if (store.selectedPeer !== 'broadcast' && ev.peer_id !== 'me') {
        mutations.bumpUnread('broadcast')
        toast.info(`${ev.peer_name}（广播）：${ev.text}`)
        flashTaskbar()
      }
      break
    case 'pigeon_trust_request':
      mutations.enqueueTrust({ peer_id: ev.peer_id, name: ev.name, fingerprint: ev.fingerprint })
      break
    case 'pigeon_fingerprint':
      store.fingerprints[ev.peer_id] = ev.fingerprint
      toast.info(`新设备已固定指纹 ${String(ev.fingerprint).slice(0, 23)}…`)
      break
    case 'pigeon_offer_incoming': {
      // 构造 pending 传输（对方发来的邀请）
      upsertFromInfo({
        id: ev.offer_id, name: ev.name, size: ev.size, files: ev.files, peer: ev.peer_id,
        direction: 'FromPhone', status: 'Waiting', transferred: 0,
      })
      const t = store.transfers[ev.offer_id]
      if (t) t.fromName = ev.peer_name
      mutations.enqueueOffer(ev.offer_id)
      toast.info(`「${ev.peer_name}」想发送「${ev.name}」`)
      break
    }
    /* ---- 远程控制 ---- */
    case 'remote_temp_password':
      mutations.setRemoteTempPassword(ev.password)
      break
    case 'remote_connecting':
      toast.info(`正在连接「${ev.name}」的远程控制…`)
      break
    case 'remote_view_ready':
      // 观看窗口由 Rust 侧自动打开；这里登记手机「只看不控」入口信息
      toast.success(`已连上「${ev.name}」的远程控制`)
      mutations.closeControlDialog()
      {
        const ip = store.meIp || (store.serverAddr || '').split(':')[0] || ''
        mutations.setRemoteViewing({ name: ev.name, view_token: ev.view_token, port: ev.port, ip })
      }
      break
    case 'remote_connect_failed':
      toast.error(`远程控制连接失败：${ev.reason || '未知原因'}`)
      break
    case 'remote_session_started':
      if (ev.side === 'controlled') {
        mutations.setRemoteControlled({ peer_id: ev.peer_id, name: ev.name })
        toast.warning(`${ev.name} 正在远程控制本机`)
        flashTaskbar()
      }
      break
    case 'remote_session_ended':
      if (ev.side === 'controlled') {
        mutations.setRemoteControlled(null)
        toast.info(`远程控制会话已结束（${ev.reason || '对端断开'}）`)
      } else if (ev.side === 'controller') {
        mutations.setRemoteViewing(null)
        toast.info(`与「${ev.name}」的远程观看已结束`)
      }
      break
    case 'remote_assist_request':
      mutations.enqueueAssist({ peer_id: ev.peer_id, name: ev.name })
      toast.info(`${ev.name} 请求远程协助控制本机`)
      flashTaskbar()
      break
  }
}

/* ---------- relayActions（对外动作，签名与 demo 一致） ---------- */

export const relayActions = {
  sendText(to: string, content: string): boolean {
    if (to === 'phone') {
      if (!store.phoneConnected) { toast.error('手机未配对，请先扫码'); return false }
      invoke('send_text', { text: content })
    } else {
      invoke('pigeon_send_text', { peerId: to, text: content })
    }
    mutations.addMessage({
      id: uuid(), from: 'me', fromName: store.meName,
      to, kind: 'text', content, createdAt: Date.now(),
    })
    return true
  },
  sendClipboard(to: string, content: string): boolean {
    return relayActions.sendText(to, content)
  },
  broadcastText(content: string): boolean {
    // 消息由 core 回显（chat_broadcast, peer_id='me'）统一入列，避免重复
    invoke('broadcast_text', { text: content })
    return true
  },
  /** 文件/文件夹发送：paths 由 Tauri 对话框/拖拽/粘贴落盘给出 */
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
    } else if (t && accepted) {
      // 接受后 FileStart 会创建正式传输行，移除邀请占位行
      mutations.removeTransfer(transferId)
    }
    try {
      invoke(accepted ? 'pigeon_accept' : 'pigeon_reject', { peerId: pid, offerId: transferId }).catch(() => {})
    } catch { /* 非 Tauri 环境（浏览器预览） */ }
  },
  /** 取消一次飞鸽传输（等待中或推送中，双向皆可；手机链路不支持） */
  cancelTransfer(transferId: string) {
    const t = store.transfers[transferId]
    if (!t) return
    if (t.from === 'phone' || t.to === 'phone') return
    const pid = t.from === 'me' ? t.to : t.from
    invoke('pigeon_cancel', { peerId: pid, offerId: transferId })
  },
  /** 删除传输记录（不影响磁盘文件） */
  forgetTransfer(transferId: string) {
    invoke('forget_transfer', { id: transferId }).catch(() => {})
    mutations.removeTransfer(transferId)
  },
  /** 删除已接收的文件并移除记录（仅限保存目录内） */
  deleteReceivedFile(transferId: string, path: string) {
    invoke('delete_received_file', { path })
      .then(() => {
        relayActions.forgetTransfer(transferId)
        toast.success('文件已删除')
      })
      .catch((e) => toast.error(`删除失败：${String(e)}`))
  },
  /** 暂停/恢复一次飞鸽推送（双向皆可发起；手机链路不支持） */
  pauseTransfer(transferId: string, paused: boolean) {
    const t = store.transfers[transferId]
    if (!t) return
    if (t.from === 'phone' || t.to === 'phone') return
    const pid = t.from === 'me' ? t.to : t.from
    invoke('pigeon_pause', { peerId: pid, offerId: transferId, paused })
  },
  /** 防打扰开关（仅已信任设备可连接、广播静音），立即生效并持久化 */
  setDnd(enabled: boolean) {
    invoke('set_dnd', { enabled })
      .then(() => mutations.setDnd(enabled))
      .catch((e) => toast.error(`切换失败：${String(e)}`))
  },
  /** 本机状态一键切换（状态菜单）：在线/免打扰/隐身互斥，两个开关一起下发 */
  setPresence(dnd: boolean, stealth: boolean) {
    invoke('set_dnd', { enabled: dnd })
      .then(() => mutations.setDnd(dnd))
      .catch((e) => toast.error(`切换防打扰失败：${String(e)}`))
    invoke('set_stealth', { enabled: stealth })
      .then(() => {
        store.stealth = stealth
      })
      .catch((e) => toast.error(`切换隐身失败：${String(e)}`))
  },
  openFolder(path: string) {
    invoke('open_folder', { path }).catch((e) => toast.error(`打开文件夹失败：${String(e)}`))
  },
  disconnectPeer(peerId: string) {
    invoke('pigeon_disconnect', { peerId })
  },
  /* ---- 自助添加主机 ---- */
  /** 登记并立即连接（重复添加等同重连） */
  addHost(addr: string, name: string) {
    invoke('pigeon_add_host', { addr, name }).catch((e) => toast.error(`添加失败：${String(e)}`))
  },
  removeHost(addr: string) {
    invoke('pigeon_remove_host', { addr }).catch((e) => toast.error(`移除失败：${String(e)}`))
  },
  connectHost(addr: string) {
    invoke('pigeon_connect_host', { addr }).catch(() => {})
  },
  /* ---- 远程控制 ---- */
  /** 发起远程控制（打开密码弹窗） */
  openControlDialog(machineId: string, name: string) {
    if (!machineId) {
      toast.error('对方 Yunshu 版本较旧，不支持远程控制')
      return
    }
    mutations.openControlDialog(machineId, name)
  },
  remoteConnect(machineId: string, password: string): boolean {
    if (!password.trim()) {
      toast.error('请输入对方机器的密码')
      return false
    }
    invoke('remote_connect', { machineId, password })
    return true
  },
  /** 断开当前远程控制会话（观看或被控） */
  remoteDisconnect() {
    invoke('remote_disconnect').catch((e) => toast.error(`断开失败：${String(e)}`))
  },
  /** 请求对方协助（对方同意后自动建连） */
  assistRequest(peerId: string) {
    invoke('remote_assist_request', { peerId })
  },
  /** 答复对方的协助请求 */
  assistRespond(peerId: string, accepted: boolean) {
    invoke('remote_assist_respond', { peerId, accepted })
    mutations.dequeueAssist(peerId)
  },
}

/* ---------- 生命周期：挂在应用根部 ---------- */

let started = false

export async function startRelay() {
  if (started) return
  started = true
  const isTauri = '__TAURI_INTERNALS__' in window

  const settings = await invoke<any>('get_settings').catch(() => ({}))
  if (settings.device_name) store.meName = settings.device_name
  mutations.setDnd(!!settings.dnd_mode)
  store.stealth = !!settings.stealth_mode

  const snap = await invoke<any>('get_state').catch(() => null)
  if (snap) {
    mutations.setServerAddr(snap.addr || '')
    mutations.setServerError(snap.error)
    mutations.setQrUrl(snap.qr_url)
    if (Array.isArray(snap.qr_alts)) mutations.setQrAlts(snap.qr_alts)
    if (snap.fingerprints) store.fingerprints = snap.fingerprints
    mutations.setPairPending(snap.pending)
    if (snap.connected) mutations.setPhoneConnected(snap.connected)
    if (snap.peers) applyPeers(snap.peers)
    for (const t of snap.transfers || []) upsertFromInfo(t)
    if (snap.remote_temp_password) mutations.setRemoteTempPassword(snap.remote_temp_password)
    if (snap.remote_controlled) mutations.setRemoteControlled(snap.remote_controlled)
  }
  if (!snap) {
    // 非 Tauri 环境（纯浏览器预览）：假装已连接，便于查看界面
    mutations.setConnection('connected')
  }

  // 微信登录态：侧栏「微信」行状态与未读提示依赖它（浏览器预览拿不到则保持未登录）
  invoke<any>('weixin_status')
    .then((s) => mutations.setWeixinLoggedIn(!!s?.logged_in))
    .catch(() => {})

  if (!isTauri) return
  // 各监听独立注册互不拖垮：曾因一处注册失败导致整个 startRelay 中断、
  // App.vue 里排在 await startRelay() 之后的 yunshu-drop 监听永远挂不上
  await listen('core-event', (e: any) => { void apply(e.payload) }).catch((e) => {
    console.error('[yunshu] core 事件监听注册失败', e)
    toast.error(`核心事件监听注册失败：${String(e).slice(0, 80)}`)
  })
  // 拖拽必须走 onDragDropEvent：裸 tauri://drag-drop 的载荷只有 paths/position，
  // enter/over/drop/leave 的 type 字段是这层封装归一化出来的，裸听永远匹配不上
  try {
    await getCurrentWebview().onDragDropEvent((e: any) => {
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
    console.log('[yunshu] 拖拽事件监听已注册')
  } catch (e) {
    console.error('[yunshu] 拖拽事件监听注册失败', e)
    toast.error(`拖拽监听注册失败：${String(e).slice(0, 80)}`)
  }
}
