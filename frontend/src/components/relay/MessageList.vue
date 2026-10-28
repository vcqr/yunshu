<script setup lang="ts">
/**
 * MessageList：消息流渲染，支持两种显示格式：
 * - bubble：气泡模式（微信式，头像 + 左右分侧气泡）
 * - list：列表模式（IRC 式，「时间 姓名： 内容」单行紧凑，信息密度高）
 * 传输卡片在两种模式下均保留功能性展示，自动滚底
 */
import { computed, nextTick, ref, watch } from 'vue'
import { MessageSquare } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import TransferCard from './TransferCard.vue'
import { formatTime } from '../../lib/relay-format'
import { store } from '../../stores/relay-store'
import { cn } from '../../lib/utils'
import type { ChatEntry } from '../../lib/relay-types'

const props = defineProps<{ entries: ChatEntry[] }>()

/* 自动滚底（条目数变化时） */
const bottomRef = ref<HTMLElement | null>(null)
const prevLen = ref(0)
watch(
  () => props.entries.length,
  () => {
    prevLen.value = props.entries.length
    void nextTick(() => bottomRef.value?.scrollIntoView({ behavior: 'smooth', block: 'end' }))
  },
  { immediate: true }
)

/* 分组：与上一条同发送者且间隔 < 3 分钟（传输卡片重置分组） */
interface RenderItem { entry: ChatEntry; grouped: boolean }
const renderItems = computed<RenderItem[]>(() => {
  const items: RenderItem[] = []
  let lastFrom: string | null = null
  let lastAt = 0
  for (const entry of props.entries) {
    if (entry.type === 'message') {
      const m = entry.message
      const grouped =
        m.from === lastFrom && m.kind !== 'system' && lastFrom !== null && m.createdAt - lastAt < 3 * 60 * 1000
      items.push({ entry, grouped })
      lastFrom = m.from
      lastAt = m.createdAt
    } else {
      items.push({ entry, grouped: false })
      lastFrom = null
      lastAt = 0
    }
  }
  return items
})

const isList = computed(() => store.chatStyle === 'list')
</script>

<template>
  <div class="custom-scroll h-full min-h-0 overflow-y-auto">
    <div :class="cn('flex min-h-full flex-col py-2.5', isList ? 'gap-0' : 'gap-0.5')">
      <!-- 空状态 -->
      <div
        v-if="!renderItems.length"
        class="flex flex-1 flex-col items-center justify-center gap-3 p-8 text-center"
      >
        <div
          class="text-emerald-600 dark:text-emerald-400 flex size-14 items-center justify-center rounded-2xl bg-gradient-to-br from-emerald-500/15 to-teal-500/10"
          aria-hidden="true"
        >
          <MessageSquare class="size-6" stroke-width="1.6" />
        </div>
        <div class="space-y-1">
          <p class="text-[13px] font-medium text-foreground">开始你的第一次对话吧</p>
          <p class="mx-auto max-w-[240px] text-[11px] leading-relaxed text-muted-foreground">
            发送消息、粘贴剪贴板、或直接把文件拖进来
          </p>
        </div>
      </div>

      <template v-for="item in renderItems" :key="item.entry.key">
        <!-- 传输卡片（两种模式均保留） -->
        <div v-if="item.entry.type === 'transfer'" class="px-2.5 sm:px-3">
          <TransferCard :transfer="item.entry.transfer" />
        </div>

        <!-- 系统消息 -->
        <div v-else-if="item.entry.message.kind === 'system'" class="px-2.5 sm:px-3">
          <p v-if="isList" class="-mx-1 px-1 py-0.5 text-[11px] leading-5 text-muted-foreground/60">
            <span class="font-mono text-[10px] tabular-nums">{{ formatTime(item.entry.message.createdAt) }}</span>
            <span class="mx-1.5">·</span>
            <span class="italic">{{ item.entry.message.content }}</span>
          </p>
          <div v-else class="flex justify-center py-0.5">
            <span class="inline-flex max-w-full truncate rounded-full bg-muted px-2 py-0.5 text-[10px] text-muted-foreground">
              {{ item.entry.message.content }}
            </span>
          </div>
        </div>

        <!-- 列表模式：IRC 式单行 -->
        <div v-else-if="isList" class="px-2.5 sm:px-3">
          <div class="group -mx-1 flex items-baseline gap-2 rounded-md px-1 py-0.5 hover:bg-muted/50">
            <time
              :datetime="new Date(item.entry.message.createdAt).toISOString()"
              class="shrink-0 font-mono text-[10px] leading-5 text-muted-foreground/70 tabular-nums"
            >
              {{ formatTime(item.entry.message.createdAt) }}
            </time>
            <span
              :class="cn(
                'shrink-0 text-[12px] font-semibold leading-5',
                item.entry.message.from === 'me'
                  ? 'text-emerald-600 dark:text-emerald-400'
                  : 'text-foreground/80'
              )"
            >
              {{ item.entry.message.from === 'me' ? '我' : item.entry.message.fromName }}
            </span>
            <p class="min-w-0 flex-1 text-[13px] leading-5 break-words whitespace-pre-wrap">
              {{ item.entry.message.content }}
            </p>
          </div>
        </div>

        <!-- 气泡模式（默认） -->
        <div v-else class="px-2.5 sm:px-3">
          <!-- 文本气泡 -->
          <div
            :class="item.entry.message.from === 'me' ? 'flex justify-end gap-2 py-0.5' : 'flex items-start justify-start gap-2 py-0.5'"
          >
            <template v-if="item.entry.message.from !== 'me'">
              <RelayAvatar
                v-if="!item.grouped"
                :name="item.entry.message.fromName"
                size="sm"
                class="mb-auto"
              />
              <div v-else class="w-7 shrink-0" aria-hidden="true"></div>
            </template>
            <div :class="cn('flex max-w-[85%] flex-col sm:max-w-[68%]', item.entry.message.from === 'me' ? 'items-end' : 'items-start')">
              <p
                v-if="!item.grouped"
                :class="cn('mb-0.5 text-[10px] font-medium', item.entry.message.from === 'me' ? 'text-muted-foreground' : 'text-foreground/70')"
              >
                {{ item.entry.message.from === 'me' ? '我' : item.entry.message.fromName }}
                <span class="ml-1.5 font-normal text-muted-foreground">{{ formatTime(item.entry.message.createdAt) }}</span>
              </p>
              <div
                :class="cn(
                  'rounded-xl px-2.5 py-1.5 shadow-sm',
                  item.entry.message.from === 'me'
                    ? 'rounded-br-sm bg-gradient-to-br from-emerald-500 to-emerald-600 text-white'
                    : 'rounded-bl-sm border bg-card text-card-foreground'
                )"
              >
                <p class="text-[13px] leading-relaxed break-words whitespace-pre-wrap">{{ item.entry.message.content }}</p>
              </div>
            </div>
          </div>
        </div>
      </template>

      <div ref="bottomRef" class="h-1 shrink-0" aria-hidden="true"></div>
    </div>
  </div>
</template>
