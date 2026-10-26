<script setup lang="ts">
import { computed } from 'vue'
import { LoaderCircle, Radio, Smartphone } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import { store, deviceHasActiveTransfer } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

const emit = defineEmits<{ (e: 'select', peer: string): void }>()

const selected = computed(() => store.selectedPeer)

const phoneOnline = computed(() => store.phoneConnected)
const phoneName = computed(() => (store.phoneConnected ? store.phoneName : '手机'))
const qrSub = computed(() => (store.phoneConnected ? '互传文件与文本' : '扫码连接 · 互传文件与文本'))

const others = computed(() => store.devices)
const activeFor = (id: string) => deviceHasActiveTransfer(id)
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-3 p-3">
    <!-- 我的设备 -->
    <div class="shrink-0">
      <h2 class="mb-2 px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">我的设备</h2>
      <div class="flex items-center gap-3 rounded-xl border bg-card p-3 shadow-sm">
        <RelayAvatar :name="store.meName" size="md" :online="true" />
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-semibold">
            {{ store.meName }}<span class="ml-1.5 text-[10px] font-medium text-muted-foreground">（我）</span>
          </p>
          <p class="truncate text-xs text-muted-foreground">{{ store.serverAddr || 'IP 获取中…' }}</p>
        </div>
      </div>
    </div>

    <!-- 手机 -->
    <div class="shrink-0">
      <h2 class="mb-2 px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">手机</h2>
      <button
        type="button"
        role="tab"
        :aria-selected="selected === 'phone'"
        :class="cn(
          'flex w-full items-center gap-3 rounded-xl border p-3 text-left transition-all',
          selected === 'phone'
            ? 'border-emerald-500/60 bg-emerald-500/10 shadow-sm'
            : 'border-transparent bg-card/60 hover:border-border hover:bg-accent'
        )"
        @click="emit('select', 'phone')"
      >
        <div
          :class="cn(
            'flex size-10 shrink-0 items-center justify-center rounded-full',
            phoneOnline ? 'bg-gradient-to-br from-emerald-400 to-teal-600 text-white' : 'bg-muted text-muted-foreground'
          )"
        >
          <Smartphone class="size-5" />
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <p class="truncate text-sm font-semibold">{{ phoneName }}</p>
            <span
              :class="cn('text-[11px] font-medium', phoneOnline ? 'text-emerald-600' : 'text-muted-foreground')"
            >{{ phoneOnline ? '在线' : '未配对' }}</span>
          </div>
          <p class="truncate text-xs text-muted-foreground">{{ qrSub }}</p>
        </div>
        <span
          v-if="activeFor('phone')"
          class="flex size-6 shrink-0 items-center justify-center rounded-full bg-emerald-500/15 text-emerald-600"
        >
          <LoaderCircle class="size-3.5 animate-spin" />
        </span>
      </button>
    </div>

    <!-- 会话：广播 -->
    <div class="shrink-0">
      <h2 class="mb-2 px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">会话</h2>
      <button
        type="button"
        role="tab"
        :aria-selected="store.selectedPeer === 'broadcast'"
        :class="cn(
          'flex w-full items-center gap-3 rounded-xl border p-3 text-left transition-all',
          store.selectedPeer === 'broadcast'
            ? 'border-emerald-500/60 bg-emerald-500/10 shadow-sm'
            : 'border-transparent bg-card/60 hover:border-border hover:bg-accent'
        )"
        @click="emit('select', 'broadcast')"
      >
        <div
          :class="cn(
            'flex size-10 shrink-0 items-center justify-center rounded-full',
            store.selectedPeer === 'broadcast'
              ? 'bg-gradient-to-br from-emerald-400 to-teal-600 text-white'
              : 'bg-muted text-muted-foreground'
          )"
        >
          <Radio class="size-5" />
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <p class="truncate text-sm font-semibold">所有人</p>
            <span class="rounded-full bg-secondary px-1.5 py-0.5 text-[10px] text-secondary-foreground">广播</span>
          </div>
          <p class="truncate text-xs text-muted-foreground">消息将发送给所有在线电脑</p>
        </div>
      </button>
    </div>

    <!-- 在线电脑 -->
    <div class="flex min-h-0 flex-1 flex-col">
      <h2 class="mb-2 shrink-0 px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">在线设备</h2>
      <div class="custom-scroll min-h-0 flex-1 overflow-y-auto pr-1">
        <div class="space-y-1.5 pb-2">
          <div
            v-if="!others.length"
            class="flex flex-col items-center gap-4 rounded-xl border border-dashed bg-card/40 px-6 py-10 text-center"
          >
            <div class="text-sm font-medium">等待其他设备上线…</div>
            <div class="max-w-[220px] text-xs leading-relaxed text-muted-foreground">
              在同一网络运行云书的设备会自动出现在这里
            </div>
          </div>
          <button
            v-for="d in others"
            :key="d.id"
            type="button"
            role="tab"
            :aria-selected="selected === d.id"
            :class="cn(
              'group flex w-full items-center gap-3 rounded-xl border p-3 text-left transition-all',
              selected === d.id
                ? 'border-emerald-500/60 bg-emerald-500/10 shadow-sm'
                : 'border-transparent bg-card/60 hover:border-border hover:bg-accent'
            )"
            @click="emit('select', d.id)"
          >
            <RelayAvatar :name="d.name" size="md" :online="d.online" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">{{ d.name }}</p>
              <p class="truncate font-mono text-xs text-muted-foreground">
                {{ d.ip }} <span v-if="store.fingerprints[d.id]" title="Noise 静态密钥已固定">🔒</span>
              </p>
            </div>
            <span
              v-if="activeFor(d.id)"
              class="flex size-6 shrink-0 items-center justify-center rounded-full bg-emerald-500/15 text-emerald-600"
            >
              <LoaderCircle class="size-3.5 animate-spin" />
            </span>
          </button>
        </div>
      </div>
    </div>

    <p class="shrink-0 text-center text-[11px] text-muted-foreground">
      {{ others.length + 1 }} 台设备在线
    </p>
  </div>
</template>
