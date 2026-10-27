<script setup lang="ts">
/**
 * TransferCard：聊天流内嵌传输卡片（发送方 / 接收方双视角）
 * 接收完成的文件可一键打开所在文件夹（云书：接收自动保存）
 */
import { computed, ref, watch } from 'vue'
import {
  CheckCircle2, ChevronRight, LoaderCircle, X,
} from '@lucide/vue'
import {
  fileIconStyle, folderIconStyle, formatBytes, formatTime, formatSpeed, transferTitle,
} from '../../lib/relay-format'
import { relayActions } from '../../hooks/use-relay'
import type { Transfer, TransferStatus } from '../../lib/relay-types'
import { cn } from '../../lib/utils'

const props = defineProps<{ transfer: Transfer }>()

const STATUS_MAP: Record<TransferStatus, { label: string; cls: string; pulse?: boolean }> = {
  pending: { label: '等待中', cls: 'border-amber-500/30 bg-amber-500/10 text-amber-600 dark:text-amber-400' },
  uploading: { label: '传输中', cls: 'border-emerald-500/30 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400', pulse: true },
  completed: { label: '已完成', cls: 'border-green-500/30 bg-green-500/10 text-green-600 dark:text-green-400' },
  rejected: { label: '已拒绝', cls: 'border-red-500/30 bg-red-500/10 text-red-600 dark:text-red-400' },
  cancelled: { label: '已取消', cls: 'border-border bg-muted text-muted-foreground' },
}
const status = computed(() => STATUS_MAP[props.transfer.status] ?? STATUS_MAP.pending)

const isSender = computed(() => props.transfer.from === 'me')
const isReceiver = computed(() => props.transfer.to === 'me')
/** 手机链路（浏览器下载/网页上传）无法从 PC 侧取消 */
const isPcTransfer = computed(() => props.transfer.from !== 'phone' && props.transfer.to !== 'phone')

const detailsOpen = ref(false)
const speed = ref(0)
const tracker = ref({ bytes: props.transfer.sentBytes, time: Date.now() })

const percent = computed(() =>
  props.transfer.totalSize > 0
    ? Math.min(100, (props.transfer.sentBytes / props.transfer.totalSize) * 100)
    : 0
)
const showProgress = computed(
  () => props.transfer.status === 'uploading' || (props.transfer.status === 'pending' && props.transfer.sentBytes > 0)
)

const titleIcon = computed(() =>
  props.transfer.kind === 'folder'
    ? folderIconStyle()
    : fileIconStyle(props.transfer.files[0]?.name ?? '')
)

/* 速度计算：按 sentBytes delta（watch 触发即本次进度事件） */
watch(
  () => props.transfer.sentBytes,
  () => {
    const now = Date.now()
    const prev = tracker.value
    const dt = now - prev.time
    const db = props.transfer.sentBytes - prev.bytes
    if (dt > 200 && db >= 0) {
      speed.value = speed.value * 0.3 + ((db * 1000) / dt) * 0.7
      tracker.value = { bytes: props.transfer.sentBytes, time: now }
    } else if (db > 0) {
      tracker.value = { bytes: props.transfer.sentBytes, time: now }
    }
  }
)
</script>

<template>
  <div :class="cn('rounded-xl border bg-card p-2.5 shadow-sm', isSender && 'border-emerald-500/30')">
    <!-- 头部 -->
    <div class="flex items-start gap-2.5">
      <span
        :class="cn('flex size-8 shrink-0 items-center justify-center rounded-lg', titleIcon.className)"
        aria-hidden="true"
      >
        <component :is="titleIcon.icon" class="size-4" />
      </span>
      <div class="min-w-0 flex-1">
        <p class="truncate text-[13px] font-semibold" :title="transferTitle(transfer.kind, transfer.files)">
          {{ transferTitle(transfer.kind, transfer.files) }}
        </p>
        <p class="mt-0.5 text-[11px] text-muted-foreground">
          {{ isSender ? `→ ${transfer.toName}` : `← ${transfer.fromName}` }}
          · {{ formatBytes(transfer.totalSize) }}
          <template v-if="transfer.files.length > 1">· {{ transfer.files.length }} 个文件</template>
          · <time :datetime="new Date(transfer.createdAt).toISOString()" class="tabular-nums">{{ formatTime(transfer.createdAt) }}</time>
        </p>
      </div>
      <span
        :class="cn('inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px]', status.cls)"
        :aria-label="`状态：${status.label}`"
      >
        <span v-if="status.pulse" class="relative flex size-1.5">
          <span class="absolute inline-flex size-full animate-ping rounded-full bg-emerald-500 opacity-75"></span>
          <span class="relative inline-flex size-1.5 rounded-full bg-emerald-500"></span>
        </span>
        {{ status.label }}
      </span>
    </div>

    <!-- 文件明细折叠 -->
    <div v-if="transfer.files.length > 0">
      <button
        class="text-muted-foreground hover:text-foreground mt-1.5 flex w-full items-center gap-1 rounded-md px-1 py-0.5 text-[11px] transition-colors"
        @click="detailsOpen = !detailsOpen"
      >
        <ChevronRight :class="cn('size-3 transition-transform', detailsOpen && 'rotate-90')" aria-hidden="true" />
        {{ detailsOpen ? '收起明细' : `查看明细（${transfer.files.length} 项）` }}
      </button>
      <div v-show="detailsOpen" class="mt-1 rounded-lg bg-muted/30 p-1">
        <ul class="space-y-1" role="list">
          <li
            v-for="f in transfer.files"
            :key="f.id"
            class="flex items-center gap-2 rounded-md px-1.5 py-1 text-[11px] transition-colors hover:bg-muted/50"
          >
            <span
              :class="cn('flex size-5 shrink-0 items-center justify-center rounded-md', fileIconStyle(f.name).className)"
              aria-hidden="true"
            >
              <component :is="fileIconStyle(f.name).icon" class="size-3" />
            </span>
            <div class="min-w-0 flex-1">
              <p class="truncate font-medium">{{ f.name }}</p>
            </div>
            <span class="shrink-0 text-[10px] text-muted-foreground">{{ formatBytes(f.size) }}</span>
          </li>
        </ul>
      </div>
    </div>

    <!-- 进度 -->
    <div v-if="showProgress" class="mt-1.5 space-y-1">
      <div
        class="h-2 w-full overflow-hidden rounded-full bg-emerald-500/10"
        role="progressbar"
        :aria-valuenow="Math.round(percent)"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-label="传输进度"
      >
        <div
          class="transfer-stripes h-full rounded-full transition-all"
          :style="{ width: Math.min(100, Math.max(0.5, percent)) + '%' }"
        ></div>
      </div>
      <div class="flex items-center justify-between text-[10px] text-muted-foreground">
        <span>{{ formatBytes(transfer.sentBytes) }} / {{ formatBytes(transfer.totalSize) }}</span>
        <span class="tabular-nums">{{ percent.toFixed(1) }}% · {{ transfer.status === 'uploading' ? formatSpeed(speed) : '' }}</span>
      </div>
    </div>

    <!-- 操作区 -->
    <div class="mt-2">
      <div v-if="isSender && transfer.status === 'pending'" class="flex items-center gap-2 text-[11px] text-muted-foreground">
        <LoaderCircle class="size-3.5 animate-spin text-amber-500" />
        等待对方接受…
      </div>
      <div v-if="isSender && transfer.status === 'uploading'" class="flex items-center gap-2 text-[11px] text-muted-foreground">
        <LoaderCircle class="size-3.5 animate-spin text-emerald-500" />
        正在上传
        <button
          v-if="isPcTransfer"
          class="ml-auto flex h-7 items-center gap-1 rounded-md border px-2 text-[11px] text-red-600 transition-colors hover:bg-red-500/10 dark:text-red-400"
          @click="relayActions.cancelTransfer(transfer.id)"
        >
          <X class="size-3" />
          取消
        </button>
      </div>
      <div v-if="isSender && transfer.status === 'completed'" class="flex items-center gap-2 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
        <CheckCircle2 class="size-3.5" />
        对方已接收
      </div>
      <div v-if="isSender && transfer.status === 'rejected'" class="text-[11px] text-red-500">
        对方拒绝了本次传输
      </div>

      <div v-if="isReceiver && transfer.status === 'pending'" class="flex items-center gap-2 text-[11px] text-muted-foreground">
        收到传输请求，请在弹窗中处理…
      </div>
      <div v-if="isReceiver && transfer.status === 'uploading'" class="flex items-center gap-2 text-[11px] text-muted-foreground">
        <LoaderCircle class="size-3.5 animate-spin text-emerald-500" />
        对方正在发送，接收中…
        <button
          v-if="isPcTransfer"
          class="ml-auto flex h-7 items-center gap-1 rounded-md border px-2 text-[11px] text-red-600 transition-colors hover:bg-red-500/10 dark:text-red-400"
          @click="relayActions.cancelTransfer(transfer.id)"
        >
          <X class="size-3" />
          取消
        </button>
      </div>
      <div v-if="isReceiver && transfer.status === 'completed'" class="text-[11px]">
        <div class="flex items-center gap-2 font-medium text-emerald-600 dark:text-emerald-400">
          <CheckCircle2 class="size-3.5" />
          已保存完成
        </div>
        <div v-if="transfer.path" class="mt-1 flex items-center gap-2 rounded-lg bg-muted/40 px-2 py-1.5">
          <span class="min-w-0 flex-1 truncate text-[10px] text-muted-foreground" :title="transfer.path">{{ transfer.path }}</span>
          <button
            class="shrink-0 rounded-md px-2 py-0.5 text-[11px] font-medium text-emerald-600 transition-colors hover:bg-emerald-500/10 dark:text-emerald-400"
            @click="relayActions.openFolder(transfer.path!)"
          >
            打开所在文件夹
          </button>
          <button
            class="shrink-0 rounded-md px-2 py-0.5 text-[11px] font-medium text-red-600 transition-colors hover:bg-red-500/10 dark:text-red-400"
            @click="relayActions.deleteReceivedFile(transfer.id, transfer.path!)"
          >
            删除文件
          </button>
        </div>
      </div>
      <div v-if="isReceiver && transfer.status === 'rejected'" class="text-[11px] text-muted-foreground">
        你已拒绝该传输
      </div>
      <div v-if="transfer.status === 'cancelled'" class="flex items-center gap-2 text-[11px] text-muted-foreground">
        <X class="size-3.5" />
        {{ isReceiver && !transfer.path ? '你已取消该传输' : isSender ? '传输已取消' : '传输失败或已取消' }}
        <button
          class="ml-auto flex h-7 items-center gap-1 rounded-md border px-2 text-[11px] transition-colors hover:bg-accent"
          @click="relayActions.forgetTransfer(transfer.id)"
        >
          删除记录
        </button>
      </div>
    </div>
  </div>
</template>
