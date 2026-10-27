<script setup lang="ts">
/**
 * TransferPanel：传输任务面板
 * lg+ 常驻第三栏（带收起按钮）/ 小屏标题栏浮层复用
 * 进行中 / 已完成 Tabs + 精简任务卡
 */
import { computed, ref } from 'vue'
import {
  ArrowDownToLine, ArrowUpFromLine, CheckCircle2, FolderOpen, Inbox, LoaderCircle, PanelRightClose, Pause, Play, X,
} from '@lucide/vue'
import { formatBytes, formatTime, transferTitle } from '../../lib/relay-format'
import { splitTransfers } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { cn } from '../../lib/utils'
import type { Transfer } from '../../lib/relay-types'

const props = defineProps<{ onCollapse?: () => void }>()

const tab = ref<'active' | 'finished'>('active')
const split = computed(() => splitTransfers())
const active = computed(() => split.value.active)
const finished = computed(() => split.value.finished)
const rows = computed(() => (tab.value === 'active' ? active.value : finished.value))
const all = computed(() => [...active.value, ...finished.value])
const totalBytes = computed(() => all.value.reduce((sum, t) => sum + t.totalSize, 0))

/* ------------------------------ 精简任务卡 ------------------------------ */
/* 以内联组件实现（Vue SFC 单文件，拆两个小组件用 render 函数成本高，直接内联循环体） */
function cardMeta(t: Transfer): string {
  const name = t.from === 'me' ? (t.toName || '对方') : t.fromName
  return `${t.from === 'me' ? '→' : '←'} ${name} · ${formatBytes(t.totalSize)} · ${formatTime(t.createdAt)}`
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <!-- 头部统计（lg+ 面板模式带收起按钮） -->
    <div class="flex h-10 shrink-0 items-center gap-1 border-b px-3">
      <h2 class="text-xs font-semibold">传输任务</h2>
      <p class="text-[10px] text-muted-foreground tabular-nums">
        {{ all.length }} 个 · {{ formatBytes(totalBytes) }}
      </p>
      <button
        v-if="onCollapse"
        aria-label="收起传输面板"
        title="收起传输面板"
        class="text-muted-foreground hover:text-foreground ml-auto hidden size-7 items-center justify-center rounded-md transition-colors hover:bg-accent xl:inline-flex"
        @click="onCollapse()"
      >
        <PanelRightClose class="size-3.5" />
      </button>
    </div>

    <!-- Tabs -->
    <div class="flex min-h-0 flex-1 flex-col gap-0">
      <div class="shrink-0 px-3 pt-2">
        <div class="bg-muted grid h-8 w-full grid-cols-2 items-center rounded-lg p-[3px]">
          <button
            :class="cn(
              'flex h-full items-center justify-center gap-1 rounded-md text-xs transition-colors',
              tab === 'active' ? 'bg-background shadow-sm' : 'text-muted-foreground hover:text-foreground'
            )"
            @click="tab = 'active'"
          >
            进行中
            <span
              v-if="active.length > 0"
              class="ml-0.5 rounded-full bg-emerald-500 px-1.5 text-[9px] font-bold text-white"
            >{{ active.length }}</span>
          </button>
          <button
            :class="cn(
              'flex h-full items-center justify-center rounded-md text-xs transition-colors',
              tab === 'finished' ? 'bg-background shadow-sm' : 'text-muted-foreground hover:text-foreground'
            )"
            @click="tab = 'finished'"
          >
            已完成
          </button>
        </div>
      </div>

      <div class="custom-scroll min-h-0 flex-1 overflow-y-auto">
        <div class="space-y-1.5 p-2.5">
          <div v-if="rows.length === 0" class="flex flex-col items-center justify-center gap-2.5 px-6 py-12 text-center">
            <div class="bg-muted text-muted-foreground flex size-11 items-center justify-center rounded-xl" aria-hidden="true">
              <Inbox class="size-5" stroke-width="1.6" />
            </div>
            <p class="text-xs text-muted-foreground">
              {{ tab === 'active' ? '暂无传输任务' : '还没有完成的传输' }}
            </p>
          </div>

          <div
            v-for="t in rows"
            :key="t.id"
            class="rounded-lg border bg-card p-2.5"
          >
            <div class="flex items-center gap-2">
              <span
                :class="cn(
                  'flex size-7 shrink-0 items-center justify-center rounded-lg',
                  t.from === 'me'
                    ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                    : 'bg-teal-500/10 text-teal-600 dark:text-teal-400'
                )"
                aria-hidden="true"
              >
                <ArrowUpFromLine v-if="t.from === 'me'" class="size-3.5" />
                <ArrowDownToLine v-else class="size-3.5" />
              </span>
              <div class="min-w-0 flex-1">
                <p class="truncate text-xs font-medium" :title="transferTitle(t.kind, t.files)">
                  {{ transferTitle(t.kind, t.files) }}
                </p>
                <p class="truncate text-[10px] text-muted-foreground">{{ cardMeta(t) }}</p>
              </div>
              <LoaderCircle
                v-if="t.status === 'pending' || t.status === 'uploading'"
                class="size-3.5 shrink-0 animate-spin text-emerald-500"
                role="status"
                aria-label="传输进行中"
              />
              <CheckCircle2
                v-else-if="t.status === 'completed'"
                class="size-3.5 shrink-0 text-green-500"
                aria-label="已完成"
              />
              <button
                v-if="(t.status === 'pending' || t.status === 'uploading' || t.status === 'paused') && t.from !== 'phone' && t.to !== 'phone'"
                :aria-label="t.status === 'paused' ? '继续传输' : '暂停传输'"
                :title="t.status === 'paused' ? '继续传输' : '暂停传输'"
                class="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-amber-600 dark:hover:text-amber-400"
                @click="relayActions.pauseTransfer(t.id, t.status !== 'paused')"
              >
                <Play v-if="t.status === 'paused'" class="size-3.5" />
                <Pause v-else class="size-3.5" />
              </button>
              <button
                v-if="(t.status === 'pending' || t.status === 'uploading' || t.status === 'paused') && t.from !== 'phone' && t.to !== 'phone'"
                aria-label="取消传输"
                title="取消传输"
                class="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-red-500/10 hover:text-red-600 dark:hover:text-red-400"
                @click="relayActions.cancelTransfer(t.id)"
              >
                <X class="size-3.5" />
              </button>
              <button
                v-if="t.status !== 'pending' && t.status !== 'uploading'"
                aria-label="删除记录"
                title="删除记录"
                class="text-muted-foreground hover:text-foreground flex size-7 shrink-0 items-center justify-center rounded-md transition-colors hover:bg-accent"
                @click="relayActions.forgetTransfer(t.id)"
              >
                <X class="size-3.5" />
              </button>
              <button
                v-if="t.status === 'completed' && t.to === 'me' && t.path"
                aria-label="打开所在文件夹"
                title="打开所在文件夹"
                class="text-muted-foreground hover:text-emerald-600 dark:hover:text-emerald-400 flex size-7 shrink-0 items-center justify-center rounded-md transition-colors hover:bg-accent"
                @click="relayActions.openFolder(t.path!)"
              >
                <FolderOpen class="size-3.5" />
              </button>
            </div>
            <div
              v-if="t.status === 'uploading' && t.totalSize > 0"
              class="mt-1.5 space-y-0.5"
            >
              <div
                class="h-1 w-full overflow-hidden rounded-full bg-emerald-500/10"
                role="progressbar"
                :aria-valuenow="Math.round(Math.min(100, (t.sentBytes / t.totalSize) * 100))"
                aria-valuemin="0"
                aria-valuemax="100"
              >
                <div
                  class="transfer-stripes h-full rounded-full"
                  :style="{ width: Math.max(2, Math.min(100, (t.sentBytes / t.totalSize) * 100)) + '%' }"
                ></div>
              </div>
              <p class="text-right text-[9px] tabular-nums text-muted-foreground">
                {{ ((t.sentBytes / t.totalSize) * 100).toFixed(1) }}%
              </p>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
