<script setup lang="ts">
import { computed } from 'vue'
import { Check, FileText, Folder, X } from '@lucide/vue'
import { fileIconStyle, folderIconStyle, formatBytes } from '../../lib/relay-format'
import { store } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { cn } from '../../lib/utils'

const emit = defineEmits<{ (e: 'handled'): void }>()

const currentOfferId = computed(() => store.currentOfferId)
const transfer = computed(() =>
  store.currentOfferId ? store.transfers[store.currentOfferId] : undefined
)
const open = computed(() => Boolean(transfer.value && transfer.value.status === 'pending'))

const files = computed(() => transfer.value?.files ?? [])
const previewFiles = computed(() => files.value.slice(0, 5))
const moreCount = computed(() => files.value.length - previewFiles.value.length)

function iconFor(name: string) {
  return transfer.value?.kind === 'folder' ? folderIconStyle() : fileIconStyle(name)
}

function handleRespond(accepted: boolean) {
  if (!transfer.value) return
  relayActions.respondTransfer(transfer.value.id, accepted)
  emit('handled')
  store.dequeueOffer()
  if (accepted) toastOk()
  else toastInfo()
}
function toastOk() { /* toast 在 use-relay 内已处理 */ }
function toastInfo() { /* 同上 */ }
</script>

<template>
  <div v-if="open" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="handleRespond(false)">
    <div class="w-full max-w-md rounded-2xl border bg-card p-5 shadow-xl">
      <div class="mb-1 flex items-center gap-2.5">
        <span
          :class="cn(
            'flex size-9 shrink-0 items-center justify-center rounded-lg',
            iconFor(files[0]?.name ?? '').className
          )"
        >
          <component
            :is="transfer?.kind === 'folder' ? Folder : FileText"
            class="size-4.5"
          />
        </span>
        <span class="font-semibold">「{{ transfer?.fromName ?? '对方' }}」想给你发送{{ transfer?.kind === 'folder' ? '文件夹' : '文件' }}</span>
      </div>
      <p class="mb-3 text-sm text-muted-foreground">
        来自 {{ transfer?.fromName }} · 共 {{ files.length }} 项 · {{ formatBytes(transfer?.totalSize ?? 0) }}
      </p>

      <div class="custom-scroll max-h-56 overflow-y-auto rounded-xl border bg-muted/30 p-2">
        <div v-for="f in previewFiles" :key="f.id" class="flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm">
          <span :class="cn('flex size-6 shrink-0 items-center justify-center rounded-md', iconFor(f.name).className)">
            <component :is="iconFor(f.name).icon" class="size-3.5" />
          </span>
          <span class="min-w-0 flex-1 truncate font-medium">{{ f.name }}</span>
          <span class="shrink-0 rounded-full bg-secondary px-2 py-0.5 text-[10px] tabular-nums text-secondary-foreground">
            {{ formatBytes(f.size) }}
          </span>
        </div>
        <p v-if="moreCount > 0" class="px-2 py-1.5 text-xs text-muted-foreground">
          …以及另外 {{ moreCount }} 个文件
        </p>
      </div>

      <div class="mt-4 flex justify-end gap-2">
        <button
          class="flex h-10 items-center gap-1.5 rounded-lg border px-4 text-sm hover:bg-accent"
          @click="handleRespond(false)"
        >
          <X class="size-4" />
          拒绝
        </button>
        <button
          class="flex h-10 items-center gap-1.5 rounded-lg bg-emerald-600 px-4 text-sm font-medium text-white hover:bg-emerald-700"
          @click="handleRespond(true)"
        >
          <Check class="size-4" />
          接受
        </button>
      </div>
    </div>
  </div>
</template>
