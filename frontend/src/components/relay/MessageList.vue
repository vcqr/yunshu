<script setup lang="ts">
import { computed } from 'vue'
import { MessageSquare, Copy, ClipboardList } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import TransferCard from './TransferCard.vue'
import { store, filterEntriesForPeer } from '../../stores/relay-store'
import { formatTime } from '../../lib/relay-format'
import type { ChatEntry } from '../../lib/relay-types'
import { cn } from '../../lib/utils'

const props = defineProps<{ peer: string }>()

const entries = computed<ChatEntry[]>(() => filterEntriesForPeer(props.peer))

interface RenderItem { entry: ChatEntry; grouped: boolean }
const renderItems = computed<RenderItem[]>(() => {
  const items: RenderItem[] = []
  let lastFrom: string | null = null
  let lastAt = 0
  for (const entry of entries.value) {
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

function copyText(text: string) {
  navigator.clipboard.writeText(text).catch(() => {})
}
</script>

<template>
  <div class="custom-scroll h-full min-h-0 overflow-y-auto">
    <div class="flex min-h-full flex-col gap-1 py-4">
      <div v-if="!renderItems.length" class="flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
        <div class="flex size-20 items-center justify-center rounded-3xl bg-gradient-to-br from-emerald-500/15 to-teal-500/10 text-emerald-600">
          <MessageSquare class="size-9" />
        </div>
        <div class="space-y-1.5">
          <p class="font-medium">开始你的第一次对话吧</p>
          <p class="mx-auto max-w-[260px] text-xs leading-relaxed text-muted-foreground">
            发送消息、粘贴剪贴板、或直接把文件拖进来
          </p>
        </div>
      </div>

      <template v-for="item in renderItems" :key="item.entry.key">
        <div v-if="item.entry.type === 'message'" class="px-3 sm:px-4">
          <!-- 剪贴板卡片 -->
          <div
            v-if="item.entry.message.kind === 'clipboard'"
            :class="item.entry.message.from === 'me' ? 'flex justify-end gap-2 py-0.5' : 'flex items-start justify-start gap-2 py-0.5'"
          >
            <RelayAvatar
              v-if="item.entry.message.from !== 'me'"
              :name="item.entry.message.fromName"
              size="sm"
              class="mb-auto"
            />
            <div class="flex max-w-[85%] flex-col gap-1 sm:max-w-[70%]">
              <p v-if="!item.grouped" class="mb-1 text-xs font-medium" :class="item.entry.message.from === 'me' ? 'text-muted-foreground' : 'text-foreground/80'">
                {{ item.entry.message.from === 'me' ? '我' : item.entry.message.fromName }}
                <span class="ml-1.5 font-normal text-muted-foreground">{{ formatTime(item.entry.message.createdAt) }}</span>
              </p>
              <div class="overflow-hidden rounded-2xl border bg-card shadow-sm">
                <div class="flex items-center gap-2 border-b bg-muted/40 px-3.5 py-2">
                  <span class="flex size-6 items-center justify-center rounded-md bg-emerald-500/15 text-emerald-600">
                    <ClipboardList class="size-3.5" />
                  </span>
                  <span class="rounded-full bg-secondary px-2 py-0.5 text-[10px] text-secondary-foreground">剪贴板内容</span>
                  <span class="ml-auto text-[10px] text-muted-foreground">{{ item.entry.message.content.length }} 字符</span>
                </div>
                <pre class="custom-scroll max-h-40 overflow-auto whitespace-pre-wrap break-all px-3.5 py-2.5 font-mono text-xs leading-relaxed">{{ item.entry.message.content }}</pre>
                <div class="border-t p-2">
                  <button
                    class="flex h-8 items-center gap-1.5 rounded-md px-2 text-xs text-emerald-600 transition-colors hover:bg-emerald-500/10"
                    @click="copyText(item.entry.message.content)"
                  >
                    <Copy class="size-3.5" />
                    复制到剪贴板
                  </button>
                </div>
              </div>
            </div>
          </div>
          <!-- 文本气泡 -->
          <div
            v-else
            :class="item.entry.message.from === 'me' ? 'flex justify-end gap-2 py-0.5' : 'flex items-start justify-start gap-2 py-0.5'"
          >
            <RelayAvatar
              v-if="item.entry.message.from !== 'me' && !item.grouped"
              :name="item.entry.message.fromName"
              size="sm"
              class="mb-auto"
            />
            <div v-else-if="item.entry.message.from !== 'me'" class="w-8 shrink-0" aria-hidden="true"></div>
            <div :class="cn('flex max-w-[85%] flex-col sm:max-w-[70%]', item.entry.message.from === 'me' ? 'items-end' : 'items-start')">
              <p v-if="!item.grouped" class="mb-1 text-xs font-medium" :class="item.entry.message.from === 'me' ? 'text-muted-foreground' : 'text-foreground/80'">
                {{ item.entry.message.from === 'me' ? '我' : item.entry.message.fromName }}
                <span class="ml-1.5 font-normal text-muted-foreground">{{ formatTime(item.entry.message.createdAt) }}</span>
              </p>
              <div
                :class="cn(
                  'rounded-2xl px-3.5 py-2.5 text-sm shadow-sm',
                  item.entry.message.from === 'me'
                    ? 'rounded-br-md bg-gradient-to-br from-emerald-500 to-emerald-600 text-white'
                    : 'rounded-bl-md border bg-card text-card-foreground'
                )"
              >
                <p class="whitespace-pre-wrap break-words leading-relaxed">{{ item.entry.message.content }}</p>
              </div>
              <p v-if="item.grouped" :class="cn('mt-0.5 text-[10px] text-muted-foreground', item.entry.message.from === 'me' ? 'pr-1' : 'pl-1')">
                {{ formatTime(item.entry.message.createdAt) }}
              </p>
            </div>
          </div>
        </div>
        <!-- 传输卡片 -->
        <div v-else class="px-3 sm:px-4">
          <TransferCard :transfer="item.entry.transfer" />
        </div>
      </template>
    </div>
  </div>
</template>
