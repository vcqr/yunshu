<script setup lang="ts">
/**
 * StatusBar：桌面软件式底部状态栏（类 VS Code / 传统桌面应用）
 * 左：品牌 · 连接 · 在线统计；右：实时传输进度（lg+ 点击可展开传输面板）
 */
import { computed, onMounted, ref } from 'vue'
import { Bird, LoaderCircle } from '@lucide/vue'
import { getVersion } from '@tauri-apps/api/app'
import { formatBytes, transferTitle } from '../../lib/relay-format'
import { store, openTransferPanelPersist } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

/** 软件版本（tauri.conf.json 的 version）；纯浏览器预览拿不到则不显示 */
const version = ref('')
onMounted(() => {
  getVersion().then((v) => (version.value = v)).catch(() => {})
})

const onlineCount = computed(() => store.devices.length + (store.phoneConnected ? 1 : 0))

const activeList = computed(() =>
  Object.values(store.transfers).filter(
    (t) =>
      (t.from === 'me' || t.to === 'me') &&
      (t.status === 'pending' || t.status === 'uploading')
  ).sort((a, b) => b.createdAt - a.createdAt)
)
const active = computed(() => activeList.value[0])
const activeCount = computed(() => activeList.value.length)

const connColor = computed(() =>
  store.connection === 'connected'
    ? 'bg-emerald-500'
    : store.connection === 'connecting'
      ? 'bg-amber-500'
      : 'bg-red-500'
)
const connLabel = computed(() =>
  store.connection === 'connected' ? '已连接' : store.connection === 'connecting' ? '连接中…' : '已断开'
)

const percent = computed(() =>
  active.value && active.value.totalSize > 0
    ? Math.min(100, (active.value.sentBytes / active.value.totalSize) * 100)
    : 0
)
</script>

<template>
  <footer
    class="mt-auto flex h-7 shrink-0 items-center gap-3 border-t bg-muted/40 px-3 text-[11px] text-muted-foreground select-none"
    aria-label="状态栏"
  >
    <!-- 品牌 · 版本 -->
    <span class="flex items-center gap-1.5 font-medium">
      <Bird class="size-3 text-emerald-500" aria-hidden="true" />
      <span class="hidden sm:inline">Yunshu</span>
      <span v-if="version" class="font-normal tabular-nums text-muted-foreground/70">v{{ version }}</span>
    </span>

    <span class="h-3 w-px bg-border" aria-hidden="true" />

    <!-- 连接 -->
    <span class="flex items-center gap-1.5" role="status">
      <span :class="cn('size-1.5 rounded-full', connColor)" aria-hidden="true" />
      {{ connLabel }}
    </span>

    <!-- 防打扰标识 -->
    <span
      v-if="store.dnd"
      class="text-emerald-600 dark:text-emerald-400 flex items-center gap-1"
      title="防打扰：仅已信任设备可连接，广播消息静音"
    >
      <span class="h-3 w-px bg-border" aria-hidden="true" />
      防打扰
    </span>

    <!-- 在线设备 -->
    <span class="hidden items-center gap-1 sm:flex">
      <span class="h-3 w-px bg-border" aria-hidden="true" />
      {{ onlineCount > 0 ? `${onlineCount} 台设备在线` : '等待设备上线' }}
      <span class="hidden md:inline"> · {{ store.messages.length }} 条消息</span>
    </span>

    <!-- 右侧：传输进度（桌面软件经典状态；lg+ 点击展开传输面板） -->
    <span
      :class="cn(
        'ml-auto flex min-w-0 items-center gap-1.5',
        'pointer-events-none lg:pointer-events-auto lg:cursor-pointer'
      )"
      title="打开传输面板"
      @click="openTransferPanelPersist()"
    >
      <template v-if="active">
        <LoaderCircle class="size-3 shrink-0 animate-spin text-emerald-500" aria-hidden="true" />
        <span class="hidden truncate sm:inline" :title="transferTitle(active.kind, active.files)">
          {{ transferTitle(active.kind, active.files) }}
        </span>
        <span class="shrink-0 tabular-nums">
          {{ formatBytes(active.sentBytes) }}/{{ formatBytes(active.totalSize) }}
        </span>
        <span class="shrink-0 font-medium tabular-nums text-emerald-600 dark:text-emerald-400">
          {{ percent.toFixed(0) }}%
        </span>
        <span v-if="activeCount > 1" class="shrink-0 text-muted-foreground">+{{ activeCount - 1 }}</span>
      </template>
      <span v-else class="text-muted-foreground/70">就绪</span>
    </span>
  </footer>
</template>
