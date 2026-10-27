/**
 * 远程观看窗口：canvas 渲染 JPEG 帧流 + 键鼠捕获 → 本地观看桥（/remote-view-ws）。
 * 独立窗口页（无 Vue 依赖）；凭证经 get_view_info 获取，不进 URL。
 * Ctrl/Cmd+V 走专用粘贴通道（先同步本机剪贴板到对方，再由对方注入平台粘贴组合键）。
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
const fullscreenBtn = document.getElementById('fullscreen')

let ws = null
let closedByUs = false
let imgW = 0
let imgH = 0
let peerName = '远程控制'
/* 帧绘制：只保留最新一帧，rAF 节流到刷新率 */
let latestBitmap = null
let frameCount = 0
let drawRect = { x: 0, y: 0, w: 1, h: 1 }

function showMask(text, spinning) {
  mask.classList.add('show')
  maskText.textContent = text
  spinner.style.display = spinning ? '' : 'none'
}
function hideMask() {
  mask.classList.remove('show')
}

function redraw() {
  if (!ctx2d) return
  ctx2d.fillStyle = '#0b0f0e'
  ctx2d.fillRect(0, 0, canvas.width, canvas.height)
  if (latestBitmap && imgW && imgH) {
    // letterbox 等比缩放
    const scale = Math.min(canvas.width / imgW, canvas.height / imgH)
    const w = imgW * scale
    const h = imgH * scale
    drawRect = { x: (canvas.width - w) / 2, y: (canvas.height - h) / 2, w, h }
    ctx2d.imageSmoothingEnabled = true
    ctx2d.imageSmoothingQuality = 'low'
    ctx2d.drawImage(latestBitmap, drawRect.x, drawRect.y, w, h)
  }
}

function rafLoop() {
  if (latestBitmap) {
    redraw()
    latestBitmap.close && latestBitmap.close()
    latestBitmap = null
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
  statusEl.textContent = ws && ws.readyState === 1 ? `${peerName} · ${frameCount} fps` : '未连接'
  frameCount = 0
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

/* ---------- 输入捕获 ---------- */
let lastMoveSent = 0
canvas.addEventListener('pointermove', (e) => {
  // 移动事件合流：最多 ~60/s，保证拖拽流畅
  const now = performance.now()
  if (now - lastMoveSent < 16) return
  lastMoveSent = now
  const { x, y } = normPos(e)
  send({ t: 'mouse_move', x, y })
})
canvas.addEventListener('pointerdown', (e) => {
  canvas.focus()
  const btn = e.button === 1 ? 1 : e.button === 2 ? 2 : 0
  send({ t: 'mouse_down', btn })
})
canvas.addEventListener('pointerup', (e) => {
  const btn = e.button === 1 ? 1 : e.button === 2 ? 2 : 0
  send({ t: 'mouse_up', btn })
})
canvas.addEventListener('contextmenu', (e) => e.preventDefault())
canvas.addEventListener('wheel', (e) => {
  e.preventDefault()
  send({ t: 'wheel', dy: e.deltaY })
}, { passive: false })

/* 键盘：全部转发到对方（含系统组合键）；Ctrl/Cmd+V 走剪贴板同步通道 */
window.addEventListener('keydown', (e) => {
  if (e.key === 'F11') {
    e.preventDefault()
    toggleFullscreen()
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
  // 修饰键左右区分（KeyboardEvent.code）
  if (e.key === 'Control') return e.code === 'ControlRight' ? 'ControlRight' : 'ControlLeft'
  if (e.key === 'Shift') return e.code === 'ShiftRight' ? 'ShiftRight' : 'ShiftLeft'
  if (e.key === 'Alt') return e.code === 'AltRight' ? 'AltRight' : 'AltLeft'
  if (e.key === 'Meta') return e.code === 'MetaRight' ? 'MetaRight' : 'MetaLeft'
  return ''
}

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
qualitySel.addEventListener('change', () => {
  send({ t: 'quality', max_width: Number(qualitySel.value), quality: 70 })
})

/* ---------- 主流程 ---------- */
async function main() {
  const isTauri = '__TAURI_INTERNALS__' in window
  if (!isTauri) {
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
  hint.textContent = `${info.name} · Ctrl/Cmd+V 粘贴本机剪贴板 · F11 全屏`
  overlay.style.display = ''

  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  ws = new WebSocket(`${proto}://127.0.0.1:${info.port}/remote-view-ws?token=${info.token}`)
  ws.binaryType = 'arraybuffer'

  ws.onopen = () => {
    hideMask()
    dot.classList.remove('off')
    send({ t: 'quality', max_width: Number(qualitySel.value), quality: 70 })
  }
  ws.onmessage = (ev) => {
    if (ev.data instanceof ArrayBuffer) {
      frameCount++
      createImageBitmap(new Blob([ev.data], { type: 'image/jpeg' }))
        .then((bmp) => {
          if (latestBitmap) latestBitmap.close && latestBitmap.close()
          latestBitmap = bmp
          imgW = bmp.width
          imgH = bmp.height
        })
        .catch(() => {})
    } else {
      try {
        const m = JSON.parse(ev.data)
        if (m.t === 'clipboard') {
          statusEl.textContent = `${peerName} · 已同步对方剪贴板`
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

window.addEventListener('resize', layout)
window.addEventListener('beforeunload', () => {
  // 窗口关闭即结束会话（core 侧也会因桥断开收尾，双保险）
  try {
    if (ws && ws.readyState === 1) ws.send(JSON.stringify({ t: 'end' }))
  } catch { /* 忽略 */ }
})
layout()
canvas.focus()
main()
