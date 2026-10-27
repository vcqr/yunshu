<script setup lang="ts">
/**
 * OfferDialog：接收方确认传输请求（队列逐个弹出）
 * 支持稍后处理（Esc / 遮罩 / X）：仅关闭弹窗，请求保留在传输列表中随时可接受/拒绝
 */
import { computed, onMounted, onUnmounted } from 'vue'
import { Check, FileText, Folder, X } from '@lucide/vue'
import { fileIconStyle, folderIconStyle, formatBytes } from '../../lib/relay-format'
import { store, mutations } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { cn } from '../../lib/utils'

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
  const t = transfer.value
  if (!t) return
  mutations.dequeueOffer()
  relayActions.respondTransfer(t.id, accepted)
  if (accepted) toast.success('已接受，等待对方发送…')
  else toast.info('已拒绝该传输')
}

/** 稍后处理：仅关闭弹窗，请求保留在传输列表中随时可接受/拒绝 */
function handleDismiss() {
  mutations.dequeueOffer()
  toast.info('已稍后处理，可在传输列表中继续操作')
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape' && open.value) handleDismiss()
}
onMounted(() => window.addEventListener('keydown', onKeydown))
onUnmounted(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div v-if="open" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="handleDismiss">
    <div class="bg-card relative w-full max-w-md rounded-xl border p-5 shadow-xl">
      <button
        aria-label="稍后处理"
        title="稍后处理：关闭弹窗，请求保留在传输列表中"
        class="text-muted-foreground hover:text-foreground absolute top-3 right-3 flex size-7 items-center justify-center rounded-md transition-colors hover:bg-accent"
        @click="handleDismiss"
      >
        <X class="size-4" />
      </button>
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
        <span class="min-w-0 font-semibold">「{{ transfer?.fromName ?? '对方' }}」想给你发送{{ transfer?.kind === 'folder' ? '文件夹' : '文件' }}</span>
      </div>
      <p class="mb-3 text-sm text-muted-foreground">
        来自 {{ transfer?.fromName }} · 共 {{ files.length }} 项 · {{ formatBytes(transfer?.totalSize ?? 0) }}
      </p>

      <div class="custom-scroll max-h-56 overflow-y-auto rounded-xl border bg-muted/30 p-2" role="list" aria-label="文件列表预览">
        <div v-for="f in previewFiles" :key="f.id" class="flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm" role="listitem">
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
          class="flex h-10 items-center gap-1.5 rounded-lg px-3 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          title="关闭弹窗，请求保留在传输列表中随时处理"
          @click="handleDismiss"
        >
          稍后处理
        </button>
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
