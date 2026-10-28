<script setup lang="ts">
/**
 * AppHeader：桌面软件式紧凑标题栏（h-11），同时是无边框窗口的自绘标题栏
 * （data-tauri-drag-region="deep" 整条子树可拖拽，双击最大化走 onTitlebarDblclick）：
 * macOS 红绿灯 | logo + 名称（仅 Win/Linux，macOS 惯例不放）| 传输（lg+ 第三栏开关 / 小屏浮层）| 主题 | 设置 | 窗口控制
 * 连接状态只在底部状态栏展示（此前头部/底栏重复展示，已收敛）。
 */
import { computed, ref } from 'vue'
import {
  ArrowLeftRight, Bell, BellOff, Menu, Moon, PanelRight, Settings2, Sun,
} from '@lucide/vue'
import AppLogo from './AppLogo.vue'
import TrafficLights from './TrafficLights.vue'
import TransferPanel from './TransferPanel.vue'
import { store, toggleTransferPanelPersist } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toggleTheme } from '../../lib/theme'
import { cn } from '../../lib/utils'
import {
  appWin, IS_MAC, isMax,
  minimize, toggleMaximize, closeWindow,
  onTitlebarDblclick,
} from '../../lib/window-controls'

const emit = defineEmits<{
  (e: 'open-settings'): void
  (e: 'toggle-sidebar'): void
}>()

const activeCount = computed(
  () => Object.values(store.transfers).filter(
    (t) =>
      (t.from === 'me' || t.to === 'me') &&
      (t.status === 'pending' || t.status === 'uploading')
  ).length
)
const badge = computed(() =>
  activeCount.value > 0
    ? activeCount.value > 9 ? '9+' : String(activeCount.value)
    : ''
)

const transferOpen = ref(false)
</script>

<template>
  <header
    data-tauri-drag-region="deep"
    class="bg-background/80 sticky top-0 z-40 flex h-11 shrink-0 select-none items-center gap-2 border-b px-2 backdrop-blur-md sm:px-3"
    @dblclick="onTitlebarDblclick($event, toggleMaximize)"
  >
    <!-- macOS 红绿灯（仅 macOS + Tauri 壳渲染；Windows/Linux 自隐藏） -->
    <TrafficLights class="mr-1" />

    <button
      aria-label="打开设备列表"
      class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground md:hidden"
      @click="emit('toggle-sidebar')"
    >
      <Menu class="size-4.5" />
    </button>

    <!-- macOS 惯例：窗口标题栏不放 logo/应用名（应用名在系统菜单栏），Windows/Linux 保留 -->
    <template v-if="!IS_MAC">
      <AppLogo size="sm" />
      <span class="text-sm font-semibold tracking-tight">云书</span>
    </template>

    <div class="ml-auto flex items-center gap-0.5">
      <!-- 传输任务 — 小屏：浮层 -->
      <div class="relative lg:hidden">
        <button
          :aria-label="`传输任务（${activeCount} 个进行中）`"
          title="传输任务"
          :class="cn(
            'relative flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors',
            'hover:bg-accent hover:text-foreground',
            transferOpen && 'bg-accent text-foreground'
          )"
          @click="transferOpen = !transferOpen"
        >
          <ArrowLeftRight class="size-4" />
          <span
            v-if="badge"
            aria-hidden="true"
            class="animate-pulse absolute top-0.5 right-0.5 flex size-3.5 items-center justify-center rounded-full bg-emerald-500 text-[9px] font-bold text-white"
          >{{ badge }}</span>
        </button>
        <!-- 小屏浮层挂在标题栏子树内：false 豁免拖拽/双击最大化（点击外部关闭的遮罩同理） -->
        <div
          v-if="transferOpen"
          class="fixed inset-0 z-40"
          data-tauri-drag-region="false"
          @click="transferOpen = false"
        ></div>
        <div
          v-if="transferOpen"
          class="bg-popover absolute top-11 right-0 z-50 h-[380px] w-88 overflow-hidden rounded-xl border shadow-lg"
          data-tauri-drag-region="false"
          aria-label="传输任务面板"
        >
          <TransferPanel />
        </div>
      </div>

      <!-- 传输任务 — lg+：右侧第三栏开关（桌面软件习惯） -->
      <button
        :aria-label="store.transferPanelOpen ? '收起传输面板' : '展开传输面板'"
        :aria-pressed="store.transferPanelOpen"
        :title="store.transferPanelOpen ? '收起传输面板' : '展开传输面板'"
        :class="cn(
          'relative hidden size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors lg:inline-flex',
          'hover:bg-accent hover:text-foreground',
          store.transferPanelOpen && 'bg-accent text-accent-foreground hover:bg-accent hover:text-accent-foreground'
        )"
        @click="toggleTransferPanelPersist()"
      >
        <PanelRight class="size-4" />
        <span
          v-if="badge"
          aria-hidden="true"
          class="animate-pulse absolute top-0.5 right-0.5 flex size-3.5 items-center justify-center rounded-full bg-emerald-500 text-[9px] font-bold text-white"
        >{{ badge }}</span>
      </button>

      <!-- 防打扰开关 -->
      <button
        :aria-label="store.dnd ? '关闭防打扰' : '开启防打扰'"
        :aria-pressed="store.dnd"
        :title="store.dnd ? '防打扰已开启：仅已信任设备可连接，广播已静音（点击关闭）' : '开启防打扰：仅已信任设备可连接，广播消息静音'"
        :class="cn(
          'flex size-9 items-center justify-center rounded-lg transition-colors',
          store.dnd
            ? 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'
            : 'text-muted-foreground hover:bg-accent hover:text-foreground'
        )"
        @click="relayActions.setDnd(!store.dnd)"
      >
        <BellOff v-if="store.dnd" class="size-4" />
        <Bell v-else class="size-4" />
      </button>

      <!-- 主题切换 -->
      <button
        aria-label="切换深浅色模式"
        title="切换深浅色模式"
        class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        @click="toggleTheme()"
      >
        <Sun class="size-4 dark:hidden" aria-hidden="true" />
        <Moon class="hidden size-4 dark:block" aria-hidden="true" />
      </button>

      <button
        aria-label="设置"
        title="设置"
        class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        @click="emit('open-settings')"
      >
        <Settings2 class="size-4" />
      </button>

      <!-- Windows/Linux 方块窗口按钮：只在 Tauri 壳里渲染；浏览器里不显示（关闭=隐藏进托盘，退出走托盘菜单） -->
      <template v-if="appWin && !IS_MAC">
        <span class="mx-1 h-4 w-px bg-border" aria-hidden="true" />
        <button
          aria-label="最小化"
          title="最小化"
          class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          @click="minimize"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M5 12h14" /></svg>
        </button>
        <button
          :aria-label="isMax ? '还原' : '最大化'"
          :title="isMax ? '还原' : '最大化'"
          class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          @click="toggleMaximize"
        >
          <svg v-if="isMax" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><path d="M9 9h10v10H9z" /><path d="M5 15V5h10" /></svg>
          <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><path d="M6 6h12v12H6z" /></svg>
        </button>
        <button
          aria-label="关闭"
          title="关闭（隐藏到托盘）"
          class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-[#e04444] hover:text-white"
          @click="closeWindow"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M6 6l12 12M18 6 6 18" /></svg>
        </button>
      </template>
    </div>
  </header>
</template>
