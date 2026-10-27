<script setup lang="ts">
/**
 * macOS 风格窗口控制（红绿灯）。仅 macOS 且在 Tauri 壳内渲染；
 * 颜色/尺寸对齐系统标准（12px 圆点、8px 间距）。
 * 绿灯：点击=全屏（系统默认，Alt+点击=最大化），悬停弹出等效 Sequoia「Move & Resize」菜单
 * （半屏平铺/最大化/全屏——系统原生的该菜单属于 NSWindow 标准按钮，自绘按钮无法
 * 触发，这里用 setPosition/setSize + workArea 实现同等动作）。
 * Windows/Linux 仍用各自的方块按钮，不由本组件渲染。
 */
import { computed } from 'vue'
import {
  appWin, IS_MAC, isMax, isFull,
  minimize, toggleMaximize, toggleFullscreen, tileWindow, closeWindow,
} from '../../lib/window-controls'

const visible = computed(() => IS_MAC && appWin != null)

function onZoomClick(e: MouseEvent): void {
  if (!appWin) return
  if (e.altKey) toggleMaximize()
  else toggleFullscreen()
}

const zoomOptions = computed(() => [
  { label: isFull.value ? '退出全屏' : '进入全屏', key: 'fullscreen' },
  { label: '移到左半屏', key: 'left' },
  { label: '移到右半屏', key: 'right' },
  { label: isMax.value ? '还原' : '最大化', key: 'maximize' },
])
function onZoomSelect(key: string): void {
  if (key === 'fullscreen') toggleFullscreen()
  else if (key === 'left' || key === 'right') void tileWindow(key)
  else toggleMaximize()
}
</script>

<template>
  <div v-if="visible" class="tlights">
    <button class="tlight close" title="关闭" @click="closeWindow">
      <svg width="8" height="8" viewBox="0 0 8 8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><path d="M1.2 1.2l5.6 5.6M6.8 1.2 1.2 6.8" /></svg>
    </button>
    <button class="tlight min" title="最小化" @click="minimize">
      <svg width="8" height="8" viewBox="0 0 8 8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><path d="M1 4h6" /></svg>
    </button>
    <div class="zoom-wrap">
      <button class="tlight zoom" title="全屏" @click="onZoomClick">
        <svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor"><path d="M1.5 1H4a.4.4 0 0 1 .28.68L2.08 3.78A.4.4 0 0 1 1.5 3.5V1zM6.5 7H4a.4.4 0 0 1-.28-.68l2.2-2.1a.4.4 0 0 1 .58.28V7z" /></svg>
      </button>
      <!-- 悬停绿灯弹出的 Move & Resize 菜单；false：菜单区域不参与标题栏拖拽/双击最大化 -->
      <div class="zoom-menu" data-tauri-drag-region="false">
        <div class="zoom-card">
          <button v-for="opt in zoomOptions" :key="opt.key" class="zoom-item" @click="onZoomSelect(opt.key)">
            {{ opt.label }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.tlights {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: none;
}
.tlight {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: none;
  padding: 0;
  display: grid;
  place-items: center;
  color: rgba(255, 255, 255, 0.9);
  cursor: pointer;
  box-shadow: inset 0 0 0 0.5px rgba(0, 0, 0, 0.18);
}
/* 系统行为：悬停红绿灯组时才显示符号，悬停的按钮加深 */
.tlight svg { opacity: 0; display: block; }
.tlights:hover .tlight svg { opacity: 1; }
.tlight.close { background: #ff5f57; }
.tlight.close:hover { background: #e0443e; }
.tlight.min { background: #febc2e; }
.tlight.min:hover { background: #e0a425; }
.tlight.zoom { background: #28c840; }
.tlight.zoom:hover { background: #1faa33; }
.tlight:focus-visible { outline: 2px solid var(--color-ring); outline-offset: 1px; }

/* 绿灯悬停菜单：外层留 9px 顶边当「桥」，指针滑向菜单时不中断 hover */
.zoom-wrap {
  position: relative;
  display: flex;
  align-items: center;
}
.zoom-menu {
  position: absolute;
  top: 100%;
  left: -5px;
  display: none;
  padding-top: 9px;
  z-index: 60;
}
.zoom-wrap:hover .zoom-menu { display: block; }
.zoom-card {
  background: var(--color-popover);
  color: var(--color-popover-foreground);
  border: 1px solid var(--color-border);
  border-radius: 10px;
  padding: 4px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.14);
  min-width: 112px;
}
.zoom-item {
  display: block;
  width: 100%;
  text-align: left;
  padding: 5px 10px;
  border: none;
  background: none;
  border-radius: 6px;
  font-size: 12px;
  line-height: 1.4;
  white-space: nowrap;
  cursor: pointer;
}
.zoom-item:hover { background: var(--color-accent); color: var(--color-accent-foreground); }
</style>
