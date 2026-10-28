/**
 * 远程观看窗口：canvas 渲染视频帧流（JPEG / H.264-WebCodecs）+ 键鼠捕获 + 文件回传。
 * 独立窗口页（无 Vue 依赖）；凭证经 get_view_info 获取，不进 URL。
 * Ctrl/Cmd+V 走专用粘贴通道（先同步本机剪贴板到对方，再由对方注入平台粘贴组合键）。
 * 帧二进制首字节为 codec（0=JPEG 1=H.264 Annex-B）；codec 切换由 core 下发。
 */
import { invoke } from '@tauri-apps/api/core'

const canvas = document.getElementById('screen')
const ctx2d = canvas.getContext('2d')
const overlay = document.getElementById('overlay')
const dot = document.getElementById('dot')
const statusEl = document.getElementById('status')
const mask = document.getElementById('mask')
const maskText = document.getElementById('maskText')
const spinner = document.getElementById('spinner')
const hint = document.getElementById('hint')
const qualitySel = document.getElementById('quality')
const resolutionSel = document.getElementById('resolution')
const monitorSel = document.getElementById('monitor')
const fullscreenBtn = document.getElementById('fullscreen')
const pill = document.getElementById('toolbar-pill')
const pillDot = document.getElementById('pill-dot')
const pillText = document.getElementById('pill-text')
const pinBtn = document.getElementById('pin-toolbar')

/* 工具栏自动隐藏：3 秒无悬停收起为小药丸，悬停展开；📌 固定后不收起 */
const AUTOHIDE_KEY = 'yunshu_viewer_toolbar_autohide'
let toolbarPinned = localStorage.getItem(AUTOHIDE_KEY) === '0'
let collapseTimer = null

function setToolbarCollapsed(collapsed) {
  if (toolbarPinned) collapsed = false
  overlay.classList.toggle('collapsed', collapsed)
  pill.classList.toggle('show', collapsed)
}
function scheduleToolbarCollapse() {
  clearTimeout(collapseTimer)
  if (toolbarPinned) return
  collapseTimer = setTimeout(() => setToolbarCollapsed(true), 3000)
}
pill.addEventListener('pointerenter', () => setToolbarCollapsed(false))
pill.addEventListener('click', () => setToolbarCollapsed(false))
overlay.addEventListener('pointerenter', () => clearTimeout(collapseTimer))
overlay.addEventListener('pointerleave', scheduleToolbarCollapse)
pinBtn.addEventListener('click', () => {
  toolbarPinned = !toolbarPinned
  localStorage.setItem(AUTOHIDE_KEY, toolbarPinned ? '1' : '0')
  pinBtn.classList.toggle('active', toolbarPinned)
  pinBtn.title = toolbarPinned ? '已固定（点击恢复自动隐藏）' : '固定工具栏（取消自动隐藏）'
  if (toolbarPinned) {
    clearTimeout(collapseTimer)
    setToolbarCollapsed(false)
  } else {
    scheduleToolbarCollapse()
  }
})

let ws = null
let closedByUs = false
let imgW = 0
let imgH = 0
let peerName = '远程控制'
/* 帧绘制：JPEG 走 ImageBitmap；H.264 走 VideoDecoder → VideoFrame，rAF 节流 */
let latestFrame = null // ImageBitmap | VideoFrame
let frameCount = 0
let drawRect = { x: 0, y: 0, w: 1, h: 1 }
/* H.264 解码 */
let videoDecoder = null
let decoderReady = false
let decoderFailed = false
let h264SeqUs = 0
let currentCodec = 0 // 0=JPEG 1=H.264（以收到的帧为准）
/* 远程软光标：受控端回传光标位置（GDI 采集不含光标），活动时隐藏本地光标 */
let remoteCursor = null // { x, y, ts }，归一化坐标
let cursorDirty = false
/* 显示缩放：1=适应窗口；>1 放大并自动跟随远程光标 */
let zoom = 1
const ZOOM_MAX = 8

function setZoom(z) {
  const next = Math.min(ZOOM_MAX, Math.max(1, z))
  if (Math.abs(next - zoom) > 1e-6) {
    zoom = next
    cursorDirty = true
  }
}

function showMask(text, spinning) {
  mask.classList.add('show')
  maskText.textContent = text
  spinner.style.display = spinning ? '' : 'none'
}
function hideMask() {
  mask.classList.remove('show')
}

function dropFrame() {
  if (latestFrame) {
    latestFrame.close && latestFrame.close()
    latestFrame = null
  }
}

/** 视图几何：自适应或按 zoom 放大；放大超窗口时跟随远程光标居中 */
function viewGeometry() {
  if (!imgW || !imgH) {
    return { w: 0, h: 0, x: 0, y: 0 }
  }
  const fit = Math.min(canvas.width / imgW, canvas.height / imgH)
  const scale = fit * zoom
  const w = imgW * scale
  const h = imgH * scale
  let x = (canvas.width - w) / 2
  let y = (canvas.height - h) / 2
  if (w > canvas.width || h > canvas.height) {
    const fx = remoteCursor ? remoteCursor.x : 0.5
    const fy = remoteCursor ? remoteCursor.y : 0.5
    x = Math.min(0, Math.max(canvas.width - w, canvas.width / 2 - fx * w))
    y = Math.min(0, Math.max(canvas.height - h, canvas.height / 2 - fy * h))
  }
  return { w, h, x, y }
}

function redraw() {
  if (!ctx2d) return
  ctx2d.fillStyle = '#0b0f0e'
  ctx2d.fillRect(0, 0, canvas.width, canvas.height)
  const g = viewGeometry()
  drawRect = g
  if (latestFrame && g.w && g.h) {
    ctx2d.imageSmoothingEnabled = zoom > 1 ? false : true // 放大用近邻保留像素感
    ctx2d.imageSmoothingQuality = 'low'
    ctx2d.drawImage(latestFrame, g.x, g.y, g.w, g.h)
  }
  drawRemoteCursor()
}

/** 远程软光标：经典箭头造型，随归一化坐标落在画面上 */
function drawRemoteCursor() {
  if (!remoteCursor || !drawRect.w) return
  const x = drawRect.x + remoteCursor.x * drawRect.w
  const y = drawRect.y + remoteCursor.y * drawRect.h
  const s = Math.max(1.1, canvas.width / 1600) // 随画布分辨率缩放
  ctx2d.save()
  ctx2d.translate(x, y)
  ctx2d.scale(s, s)
  ctx2d.beginPath()
  ctx2d.moveTo(0, 0)
  ctx2d.lineTo(0, 17)
  ctx2d.lineTo(4.2, 13.2)
  ctx2d.lineTo(7, 19.5)
  ctx2d.lineTo(9.4, 18.4)
  ctx2d.lineTo(6.6, 12.2)
  ctx2d.lineTo(12, 12)
  ctx2d.closePath()
  ctx2d.fillStyle = 'rgba(255,255,255,0.95)'
  ctx2d.strokeStyle = '#111'
  ctx2d.lineWidth = 1.6
  ctx2d.fill()
  ctx2d.stroke()
  ctx2d.restore()
}

function rafLoop() {
  // 帧位图保留到被新帧替换（替换路径内 dropFrame），光标移动可单独重绘
  if (latestFrame || cursorDirty) {
    cursorDirty = false
    redraw()
  }
  requestAnimationFrame(rafLoop)
}
requestAnimationFrame(rafLoop)

function layout() {
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.round(canvas.clientWidth * dpr)
  canvas.height = Math.round(canvas.clientHeight * dpr)
  redraw()
}

/* fps 统计（含 0fps 自愈：剪贴板提示等临时文案 1 秒内被覆盖） */
setInterval(() => {
  const zoomLabel = zoom > 1 ? ` · ×${zoom.toFixed(1)}` : ''
  const dimLabel = imgW && imgH ? ` · ${imgW}×${imgH}` : ''
  statusEl.textContent =
    ws && ws.readyState === 1 ? `${peerName} · ${frameCount} fps · ${currentCodec === 1 ? 'H.264' : 'JPEG'}${dimLabel}${zoomLabel}` : '未连接'
  // 收起药丸同步：fps + 连接状态点
  pillText.textContent = `${frameCount} fps`
  pillDot.className = dot.className
  frameCount = 0
  // 远程软光标 2 秒无更新（只看模式/旧版本对端）→ 清除并恢复本地光标
  if (remoteCursor && performance.now() - remoteCursor.ts > 2000) {
    remoteCursor = null
    cursorDirty = true
    canvas.classList.remove('remote-cursor')
  }
}, 1000)

/* ---------- 坐标映射（canvas 物理像素 → [0,1] 归一化） ---------- */
function normPos(e) {
  const rect = canvas.getBoundingClientRect()
  const dpr = canvas.width / rect.width
  const px = (e.clientX - rect.left) * dpr - drawRect.x
  const py = (e.clientY - rect.top) * dpr - drawRect.y
  return {
    x: Math.min(1, Math.max(0, px / drawRect.w)),
    y: Math.min(1, Math.max(0, py / drawRect.h)),
  }
}

function send(obj) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(obj))
}

/* ---------- H.264：WebCodecs 解码（Annex-B，无 description） ---------- */
function ensureDecoder() {
  if (decoderReady || decoderFailed) return decoderReady
  if (typeof VideoDecoder === 'undefined') {
    decoderFailed = true
    return false
  }
  try {
    videoDecoder = new VideoDecoder({
      output: (frame) => {
        dropFrame()
        latestFrame = frame
        imgW = frame.displayWidth
        imgH = frame.displayHeight
      },
      error: (e) => {
        console.error('WebCodecs 解码错误，回退 JPEG', e)
        decoderFailed = true
        decoderReady = false
        try { videoDecoder.close() } catch { /* 忽略 */ }
        videoDecoder = null
        send({ t: 'prefer_codec', codec: 'jpeg' }) // 回退 JPEG
      },
    })
    // Baseline L4.0：1920×1200 需 ≥L3.2，留余量到 4K 内
    videoDecoder.configure({ codec: 'avc1.420028', optimizeForLatency: true })
    decoderReady = videoDecoder.state === 'configured'
  } catch {
    decoderFailed = true
    decoderReady = false
  }
  return decoderReady
}

function feedH264(data) {
  if (!ensureDecoder() || !videoDecoder || videoDecoder.state !== 'configured') return
  const key = data.length > 4 && data[0] === 0 && data[1] === 0 && data[2] === 0 && data[3] === 1
    ? hasIdr(data)
    : false
  const chunk = new EncodedVideoChunk({
    type: key ? 'key' : 'delta',
    timestamp: h264SeqUs,
    data,
  })
  h264SeqUs += 40_000 // 25fps 名义时钟，仅保证单调
  try {
    videoDecoder.decode(chunk)
  } catch (e) {
    console.error('decode 失败', e)
  }
}

/** 扫描 Annex-B NAL：是否含 IDR（nal_type=5）/SPS(7) */
function hasIdr(data) {
  for (let i = 0; i + 4 < data.length; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && (data[i + 2] === 1 || (data[i + 2] === 0 && data[i + 3] === 1))) {
      const hdr = data[i + 2] === 1 ? i + 3 : i + 4
      if (hdr < data.length) {
        const t = data[hdr] & 0x1f
        if (t === 5 || t === 7) return true
      }
    }
  }
  return false
}

/* ---------- 输入捕获 ---------- */
let lastMoveSent = 0
canvas.addEventListener('pointermove', (e) => {
  const now = performance.now()
  if (now - lastMoveSent < 16) return
  lastMoveSent = now
  const { x, y } = normPos(e)
  send({ t: 'mouse_move', x, y })
})
canvas.addEventListener('pointerdown', (e) => {
  canvas.focus()
  if (e.button > 2) return // 侧键（前进/后退）不映射，避免误当左键注入
  const btn = e.button === 1 ? 1 : e.button === 2 ? 2 : 0
  send({ t: 'mouse_down', btn })
})
canvas.addEventListener('pointerup', (e) => {
  if (e.button > 2) return
  const btn = e.button === 1 ? 1 : e.button === 2 ? 2 : 0
  send({ t: 'mouse_up', btn })
})
canvas.addEventListener('contextmenu', (e) => e.preventDefault())
canvas.addEventListener('wheel', (e) => {
  e.preventDefault()
  // Ctrl+滚轮 = 本地显示缩放；普通滚轮 = 远程滚动
  if (e.ctrlKey || e.metaKey) {
    setZoom(zoom * Math.pow(1.1, -e.deltaY / 100))
    return
  }
  send({ t: 'wheel', dy: e.deltaY })
}, { passive: false })

/* 键盘：全部转发到对方；Ctrl/Cmd+V 走剪贴板同步通道 */
window.addEventListener('keydown', (e) => {
  if (e.key === 'F11') {
    e.preventDefault()
    toggleFullscreen()
    return
  }
  // 显示缩放快捷键（浏览器习惯）：Ctrl+0 适应窗口，Ctrl+= 放大，Ctrl+- 缩小；
  // 本地消费，不再转发远端
  if ((e.ctrlKey || e.metaKey) && (e.key === '0' || e.key === '=' || e.key === '-')) {
    e.preventDefault()
    if (e.key === '0') setZoom(1)
    else if (e.key === '=') setZoom(zoom * 1.25)
    else setZoom(zoom / 1.25)
    return
  }
  e.preventDefault()
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'v') {
    send({ t: 'clip_paste' })
    return
  }
  send({ t: 'key_down', key: keyName(e) })
})
window.addEventListener('keyup', (e) => {
  e.preventDefault()
  send({ t: 'key_up', key: keyName(e) })
})

function keyName(e) {
  if (e.key.length === 1) return e.key
  const named = new Set([
    'Enter', 'Backspace', 'Delete', 'Tab', 'Escape', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
    'Home', 'End', 'PageUp', 'PageDown', 'CapsLock', 'F1', 'F2', 'F3', 'F4', 'F5', 'F6', 'F7', 'F8', 'F9', 'F10', 'F11', 'F12',
  ])
  if (named.has(e.key)) return e.key
  if (e.key === 'Control') return e.code === 'ControlRight' ? 'ControlRight' : 'ControlLeft'
  if (e.key === 'Shift') return e.code === 'ShiftRight' ? 'ShiftRight' : 'ShiftLeft'
  if (e.key === 'Alt') return e.code === 'AltRight' ? 'AltRight' : 'AltLeft'
  if (e.key === 'Meta') return e.code === 'MetaRight' ? 'MetaRight' : 'MetaLeft'
  return ''
}

/* ---------- 文件回传：按钮 + 拖拽 ---------- */
function sendFiles(paths) {
  if (paths && paths.length) invoke('remote_send_files', { paths })
}
document.getElementById('sendfile').addEventListener('click', async () => {
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const paths = await open({ multiple: true, title: '选择要传给对方电脑的文件（多选）' })
    sendFiles(paths)
  } catch (e) {
    console.error('文件选择失败', e)
  }
})
try {
  (async () => {
    const { getCurrentWebview } = await import('@tauri-apps/api/webview')
    getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload || {}
      if (p.type === 'drop' && p.paths?.length) sendFiles(p.paths)
    })
  })()
} catch { /* 非 Tauri 环境 */ }

/* ---------- 覆盖层 ---------- */
document.getElementById('disconnect').addEventListener('click', () => {
  closedByUs = true
  send({ t: 'end' })
  showMask('已断开远程控制，可关闭本窗口', false)
  setTimeout(() => window.close(), 600)
})
fullscreenBtn.addEventListener('click', toggleFullscreen)
function toggleFullscreen() {
  if (document.fullscreenElement) {
    document.exitFullscreen().catch(() => {})
  } else {
    document.documentElement.requestFullscreen().catch(() => {})
  }
}
/** 画质档位基线（与受控端 quality_tier 一致） */
const TIER_QUALITY = { 720: 55, 1080: 65, 1920: 75 }

/** 发送画质：分辨率下拉钉住时显式指定最长边（0=原始），否则跟随画质档位 */
function sendQuality() {
  const tier = Number(qualitySel.value)
  const res = resolutionSel.value
  if (res === 'auto') {
    send({ t: 'quality', tier })
  } else {
    send({ t: 'quality', max_side: Number(res), quality: TIER_QUALITY[tier] ?? 75 })
  }
}
qualitySel.addEventListener('change', sendQuality)
resolutionSel.addEventListener('change', sendQuality)
monitorSel.addEventListener('change', () => {
  send({ t: 'monitor', index: Number(monitorSel.value) })
})

/* ---------- 主流程 ---------- */
async function main() {
  if (!('__TAURI_INTERNALS__' in window)) {
    showMask('请在云书应用内打开本窗口', false)
    return
  }
  let info
  try {
    info = await invoke('get_view_info')
  } catch (e) {
    showMask(String(e), false)
    return
  }
  peerName = info.name
  imgW = info.width
  imgH = info.height
  hint.textContent = `${info.name} · Ctrl+滚轮缩放 · Ctrl/Cmd+V 粘贴本机剪贴板 · F11 全屏 · 拖文件进窗口回传`
  overlay.style.display = ''
  pinBtn.classList.toggle('active', toolbarPinned)
  scheduleToolbarCollapse()
  fillMonitors([]) // hello 消息到达后再填充

  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  ws = new WebSocket(`${proto}://127.0.0.1:${info.port}/remote-view-ws?token=${info.token}`)
  ws.binaryType = 'arraybuffer'

  ws.onopen = () => {
    hideMask()
    dot.classList.remove('off')
    sendQuality()
    // 上报解码能力（WebCodecs 可用才启用 H.264）
    const h264 = typeof VideoDecoder !== 'undefined' && !!ensureDecoder()
    send({ t: 'caps', h264 })
  }
  ws.onmessage = (ev) => {
    if (ev.data instanceof ArrayBuffer) {
      const u8 = new Uint8Array(ev.data)
      if (u8.length < 3) return
      const codec = u8[0]
      const payload = u8.subarray(1)
      frameCount++
      currentCodec = codec
      if (codec === 1) {
        feedH264(payload)
      } else {
        createImageBitmap(new Blob([payload], { type: 'image/jpeg' }))
          .then((bmp) => {
            dropFrame()
            latestFrame = bmp
            imgW = bmp.width
            imgH = bmp.height
          })
          .catch(() => {})
      }
    } else {
      try {
        const m = JSON.parse(ev.data)
        if (m.t === 'clipboard') {
          statusEl.textContent = `${peerName} · 已同步对方剪贴板`
        } else if (m.t === 'codec') {
          // 编码切换提示（首个 H.264 关键帧到达前画面可能短暂停顿）
          if (m.codec === 'h264') ensureDecoder()
        } else if (m.t === 'cursor') {
          // 受控端光标位置 → 软光标
          remoteCursor = { x: m.x, y: m.y, ts: performance.now() }
          cursorDirty = true
          canvas.classList.add('remote-cursor')
        } else if (m.t === 'screen') {
          fillMonitors(m.monitors || [])
        } else if (m.t === 'hello') {
          fillMonitors(m.monitors || [])
        } else if (m.t === 'end') {
          dot.classList.add('off')
          showMask(`远程控制已结束：${m.reason || '对端断开'}`, false)
        }
      } catch { /* 忽略非 JSON */ }
    }
  }
  ws.onerror = () => {
    if (!closedByUs) showMask('观看通道连接失败', false)
  }
  ws.onclose = () => {
    if (!closedByUs) {
      dot.classList.add('off')
      showMask('连接已断开，可关闭本窗口', false)
    }
  }
}

function fillMonitors(monitors) {
  const cur = monitorSel.value
  monitorSel.innerHTML = ''
  // 被控端真实屏幕分辨率 → "原始"选项动态标注
  if (monitors.length && resolutionSel) {
    const raw = resolutionSel.querySelector('option[value="0"]')
    if (raw) raw.textContent = `原始（${monitors[0].width}×${monitors[0].height}）`
  }
  if (!monitors.length) {
    const opt = document.createElement('option')
    opt.value = '0'
    opt.textContent = '显示器'
    monitorSel.appendChild(opt)
    monitorSel.style.display = monitors.length ? '' : (monitorSel.dataset.has ? '' : 'none')
    return
  }
  monitorSel.dataset.has = '1'
  monitorSel.style.display = monitors.length > 1 ? '' : 'none'
  for (const m of monitors) {
    const opt = document.createElement('option')
    opt.value = String(m.index)
    opt.textContent = `${m.name}（${m.width}×${m.height}）`
    monitorSel.appendChild(opt)
  }
  if (monitors.some((m) => String(m.index) === cur)) monitorSel.value = cur
}

window.addEventListener('resize', layout)
window.addEventListener('beforeunload', () => {
  try {
    if (ws && ws.readyState === 1) ws.send(JSON.stringify({ t: 'end' }))
  } catch { /* 忽略 */ }
})
layout()
canvas.focus()
main()
