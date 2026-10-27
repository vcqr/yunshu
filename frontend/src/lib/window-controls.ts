/**
 * 无边框窗口控制（复刻 pi-dock App.vue 的自绘标题栏方案）：
 * 窗口 decorations:false，AppHeader 作为标题栏承载拖拽与双击最大化；
 * Windows/Linux 用方块窗口按钮（AppHeader 右端），macOS 用红绿灯（TrafficLights.vue）。
 * 浏览器模式（vite dev 无 Tauri 壳）appWin 为 null，所有按钮自动不渲染、动作安全降级。
 */
import { ref } from 'vue'
import { getCurrentWindow, currentMonitor, PhysicalPosition, PhysicalSize } from '@tauri-apps/api/window'

/** 运行平台是否 macOS：窗口控制按钮按平台切换风格（红绿灯 / 方块按钮） */
export const IS_MAC = typeof navigator !== 'undefined' && /^Mac/i.test(navigator.platform || navigator.userAgent)

/** Tauri 窗口句柄；纯浏览器（vite dev）里拿不到，为 null */
export const appWin = (() => {
  try {
    return getCurrentWindow()
  } catch {
    return null
  }
})()

/** 当前是否最大化（最大化/还原图标切换） */
export const isMax = ref(false)
/** 当前是否全屏（macOS 绿灯标题提示） */
export const isFull = ref(false)

async function refreshWinState(): Promise<void> {
  if (!appWin) return
  try {
    isMax.value = await appWin.isMaximized()
    isFull.value = await appWin.isFullscreen()
  } catch {
    /* not in tauri */
  }
}

/** App.vue 挂载时调用：刷新窗口状态并跟随 resize（最大化/还原、全屏状态切换图标） */
export function initWindowControls(): void {
  if (!appWin) return
  void refreshWinState()
  void appWin.onResized(() => void refreshWinState()).catch(() => {
    /* not in tauri */
  })
}

export function minimize(): void {
  void appWin?.minimize()
}
export function toggleMaximize(): void {
  void appWin?.toggleMaximize()
}
export function toggleFullscreen(): void {
  void appWin?.setFullscreen(!isFull.value)
}
export function closeWindow(): void {
  void appWin?.close()
}

/** 绿灯菜单「移到半屏」：按显示器工作区（去掉 Dock/任务栏）平铺到左/右一半 */
export async function tileWindow(side: 'left' | 'right'): Promise<void> {
  if (!appWin) return
  try {
    if (isFull.value) await appWin.setFullscreen(false)
    const mon = await currentMonitor()
    if (!mon) return
    const wa = mon.workArea
    const half = Math.floor(wa.size.width / 2)
    const x = side === 'left' ? wa.position.x : wa.position.x + half
    await appWin.setPosition(new PhysicalPosition(x, wa.position.y))
    await appWin.setSize(new PhysicalSize(half, wa.size.height))
  } catch {
    /* 显示器信息拿不到等瞬态，忽略 */
  }
}

let dblclickGuardInstalled = false
/**
 * 安装全局守卫：标题栏（data-tauri-drag-region="deep"）上的原生多次点击（双击/三击）
 * 不再交给 tauri 内建脚本处理（内建路径在 macOS 依赖 mouseup 像素级同位、且首次按下
 * 已进入原生拖拽会话，触控板轻点也不产生 detail=2，双击经常不触发），统一由标题栏的
 * @dblclick → onTitlebarDblclick 处理；拦截时同时阻止默认行为——tauri 内建脚本只在
 * 非 macOS 路径做 preventDefault，macOS 的第二次按下没有拦截，WKWebView 会从那里
 * 启动双击选词/拖选，把标题栏下方的主界面内容选亮。App.vue 挂载时调用一次。
 */
export function installTauriDblclickGuard(): void {
  if (dblclickGuardInstalled || typeof document === 'undefined') return
  dblclickGuardInstalled = true
  document.addEventListener(
    'mousedown',
    (e) => {
      if (e.detail >= 2 && (e.target as HTMLElement | null)?.closest?.('[data-tauri-drag-region="deep"]')) {
        e.preventDefault() // 阻止双击选词/三击选段等默认行为
        e.stopPropagation() // capture 阶段拦下，tauri 的 document 冒泡监听不再收到
      }
    },
    true,
  )
}

/**
 * 标题栏 @dblclick 处理：点在可交互元素（按钮/链接/输入类，或显式 data-tauri-drag-region="false"）
 * 上不触发最大化，其余区域（含状态文本、空白）等同原生标题栏双击缩放；
 * 并清掉可能已产生的选区（按下位置漂移落进内容区时浏览器已开始选择）。
 */
export function onTitlebarDblclick(e: MouseEvent, toggle: () => void): void {
  const el = e.target as HTMLElement | null
  if (el?.closest("button, a, input, select, textarea, [data-tauri-drag-region='false']")) return
  e.preventDefault()
  window.getSelection()?.removeAllRanges()
  toggle()
}
