<script setup lang="ts">
import { computed } from 'vue'
import { LoaderCircle, Settings2, WifiOff, Wifi } from '@lucide/vue'
import AppLogo from './AppLogo.vue'
import { store } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

const emit = defineEmits<{ (e: 'open-settings'): void }>()

const conn = computed(() => store.connection)
const pill = computed(() => {
  const map = {
    connected: { cls: 'border-emerald-500/30 bg-emerald-500/10 text-emerald-600', icon: Wifi, dot: 'bg-emerald-500', label: '已连接' },
    connecting: { cls: 'border-amber-500/30 bg-amber-500/10 text-amber-600', icon: LoaderCircle, dot: 'bg-amber-500', label: '连接中', spin: true },
    disconnected: { cls: 'border-red-500/30 bg-red-500/10 text-red-600', icon: WifiOff, dot: 'bg-red-500', label: '已断开' },
  } as const
  return map[conn.value]
})
</script>

<template>
  <header class="sticky top-0 z-40 flex h-14 shrink-0 items-center gap-3 border-b bg-background/80 px-4 backdrop-blur-md">
    <AppLogo />
    <div class="flex flex-col leading-tight">
      <span class="text-base font-bold tracking-tight">云书</span>
      <span class="text-[10px] font-medium uppercase tracking-widest text-muted-foreground">局域网传书</span>
    </div>
    <div class="mx-1 h-6 w-px bg-border" aria-hidden="true" />

    <!-- 连接状态 pill -->
    <div
      :class="cn('inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium', pill.cls)"
      role="status"
    >
      <component :is="pill.icon" :class="cn('size-3.5', 'spin' in pill && pill.spin && 'animate-spin')" />
      <span>{{ pill.label }}</span>
    </div>
    <span class="truncate font-mono text-xs text-muted-foreground">{{ store.serverAddr }}</span>

    <div class="ml-auto flex items-center gap-1">
      <button
        aria-label="设置"
        class="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        @click="emit('open-settings')"
      >
        <Settings2 class="size-5" />
      </button>
    </div>
  </header>
</template>
