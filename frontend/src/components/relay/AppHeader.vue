<script setup lang="ts">
/**
 * AppHeader：桌面软件式紧凑标题栏（h-11）
 * logo + 名称 | 行内连接状态 | 传输（lg+ 第三栏开关 / 小屏浮层）| 主题 | 设置
 */
import { computed, ref } from 'vue'
import {
  ArrowLeftRight, Bell, BellOff, LoaderCircle, Menu, Moon, PanelRight, Settings2, Sun, Wifi, WifiOff,
} from '@lucide/vue'
import AppLogo from './AppLogo.vue'
import TransferPanel from './TransferPanel.vue'
import { store, toggleTransferPanelPersist } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toggleTheme } from '../../lib/theme'
import { cn } from '../../lib/utils'

const emit = defineEmits<{
  (e: 'open-settings'): void
  (e: 'toggle-sidebar'): void
}>()

const conn = computed(() => store.connection)
const connLabel = computed(() =>
  conn.value === 'connected' ? '已连接' : conn.value === 'connecting' ? '连接中…' : '已断开'
)

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
  <header class="bg-background/80 sticky top-0 z-40 flex h-11 shrink-0 items-center gap-2 border-b px-2 backdrop-blur-md sm:px-3">
    <button
      aria-label="打开设备列表"
      class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground md:hidden"
      @click="emit('toggle-sidebar')"
    >
      <Menu class="size-4.5" />
    </button>

    <AppLogo size="sm" />
    <span class="text-sm font-semibold tracking-tight">云书</span>

    <div class="mx-1 hidden h-4 w-px bg-border sm:block" aria-hidden="true" />
    <span
      role="status"
      :aria-label="`连接状态：${connLabel}`"
      class="hidden items-center gap-1.5 text-xs text-muted-foreground sm:flex"
    >
      <Wifi v-if="conn === 'connected'" class="size-3 text-emerald-500" aria-hidden="true" />
      <LoaderCircle v-else-if="conn === 'connecting'" class="size-3 animate-spin text-amber-500" aria-hidden="true" />
      <WifiOff v-else class="size-3 text-red-500" aria-hidden="true" />
      {{ connLabel }}
    </span>

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
        <div
          v-if="transferOpen"
          class="fixed inset-0 z-40"
          @click="transferOpen = false"
        ></div>
        <div
          v-if="transferOpen"
          class="bg-popover absolute top-11 right-0 z-50 h-[380px] w-88 overflow-hidden rounded-xl border shadow-lg"
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
    </div>
  </header>
</template>
