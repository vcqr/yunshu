<script setup lang="ts">
import { computed, ref } from 'vue'
import { ClipboardPaste, CloudUpload, FileUp, FolderUp, MessageSquare, Plus, Send, X } from '@lucide/vue'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { store } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

const props = defineProps<{ peer: string }>()
const emit = defineEmits<{ (e: 'send-clipboard'): void }>()

const text = ref('')
const menuOpen = ref(false)
const clipboardOpen = ref(false)
const clipboardText = ref('')
const clipboardFallback = ref(false)

const isPhone = computed(() => props.peer === 'phone')
const isBroadcast = computed(() => props.peer === 'broadcast')
const peerName = computed(() =>
  isPhone.value ? store.phoneName : store.devices.find((d) => d.id === props.peer)?.name || '对方设备'
)
const peerOnline = computed(() =>
  isPhone.value ? store.phoneConnected : store.devices.find((d) => d.id === props.peer)?.online || false
)

function handleSend() {
  const content = text.value.trim()
  if (!content) return
  if (!peerOnline.value) { toast.error('对方已离线，无法发送'); return }
  const ok = isBroadcast.value
    ? relayActions.broadcastText(content)
    : relayActions.sendText(props.peer, content)
  if (ok) text.value = ''
}

/* 文件 / 文件夹：Tauri 对话框取路径，打包与推送由 core 完成 */
async function pickFiles() {
  menuOpen.value = false
  if (isBroadcast.value) { toast.error('广播会话仅支持文本，请先在左侧选择具体设备'); return }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const paths = await open({ multiple: true, title: '选择要发送的文件' })
  if (!paths?.length) return
  relayActions.sendPaths(isPhone.value ? 'phone' : 'pc', isPhone.value ? null : props.peer, paths)
}
async function pickFolder() {
  menuOpen.value = false
  if (isBroadcast.value) { toast.error('广播会话仅支持文本，请先在左侧选择具体设备'); return }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const dir = await open({ directory: true, title: '选择要发送的文件夹（将自动打包为 zip）' })
  if (!dir) return
  relayActions.sendPaths(isPhone.value ? 'phone' : 'pc', isPhone.value ? null : props.peer, [dir])
}

async function openClipboardDialog() {
  menuOpen.value = false
  if (isBroadcast.value) { toast.error('广播会话仅支持文本消息'); return }
  try {
    clipboardText.value = await navigator.clipboard.readText()
    clipboardFallback.value = false
  } catch {
    clipboardText.value = ''
    clipboardFallback.value = true
  }
  clipboardOpen.value = true
}
function confirmSendClipboard() {
  const content = clipboardText.value.trim()
  if (!content) return
  if (relayActions.sendClipboard(props.peer, content)) {
    toast.success('剪贴板内容已发送')
    clipboardOpen.value = false
    clipboardText.value = ''
  }
}

function onMenuKey(e: KeyboardEvent) {
  if (e.key === 'Escape') menuOpen.value = false
}
</script>

<template>
  <div class="shrink-0 border-t bg-card/60 p-3 backdrop-blur-sm sm:p-4" @keydown="onMenuKey">
    <div class="flex items-end gap-2">
      <!-- 附件菜单 -->
      <div class="relative">
        <button
          aria-label="添加附件"
          :class="cn(
            'flex size-10 shrink-0 items-center justify-center rounded-xl border bg-background text-muted-foreground transition-colors',
            'hover:bg-accent hover:text-foreground',
            menuOpen && 'border-ring ring-[3px] ring-ring/50'
          )"
          @click.stop="menuOpen = !menuOpen"
        >
          <Plus class="size-5" />
        </button>
        <div
          v-if="menuOpen"
          class="absolute bottom-12 left-0 z-30 w-52 overflow-hidden rounded-xl border bg-popover p-1 shadow-lg"
          @click.stop
        >
          <button
            class="flex min-h-10 w-full items-center gap-2.5 rounded-lg px-2.5 text-sm hover:bg-accent"
            @click="pickFiles"
          >
            <FileUp class="size-4 text-emerald-600" />
            发送文件
          </button>
          <button
            class="flex min-h-10 w-full items-center gap-2.5 rounded-lg px-2.5 text-sm hover:bg-accent"
            @click="pickFolder"
          >
            <FolderUp class="size-4 text-amber-500" />
            发送文件夹
          </button>
          <button
            class="flex min-h-10 w-full items-center gap-2.5 rounded-lg px-2.5 text-sm hover:bg-accent"
            @click="openClipboardDialog"
          >
            <ClipboardPaste class="size-4 text-violet-500" />
            发送剪贴板内容
          </button>
        </div>
      </div>

      <!-- 输入框 -->
      <textarea
        v-model="text"
        rows="1"
        :placeholder="isBroadcast ? '发送广播消息给所有在线电脑…' : `发送给 ${peerName}…`"
        :class="cn(
          'custom-scroll max-h-32 min-h-10 w-full resize-none rounded-xl border bg-background px-3.5 py-2.5 text-sm shadow-xs outline-none',
          'transition-[color,box-shadow] placeholder:text-muted-foreground',
          'focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50'
        )"
        @keydown.enter.exact.prevent="handleSend"
      ></textarea>

      <!-- 发送 -->
      <button
        aria-label="发送消息"
        :disabled="!text.trim()"
        :class="cn(
          'flex size-10 shrink-0 items-center justify-center rounded-xl text-white shadow-sm transition-transform',
          text.trim()
            ? 'bg-gradient-to-r from-emerald-500 to-emerald-600 shadow-emerald-500/30 hover:scale-105 hover:from-emerald-600 hover:to-emerald-700 active:scale-95'
            : 'cursor-not-allowed bg-muted text-muted-foreground'
        )"
        @click="handleSend"
      >
        <Send class="size-5" />
      </button>
    </div>

    <p class="mt-1.5 hidden px-1 text-[10px] text-muted-foreground sm:block">
      Enter 发送 · Shift+Enter 换行 · 拖拽文件到聊天区可直接发送
    </p>

    <!-- 隐藏 input（供 Tauri 对话框不可用时的兜底；桌面端主用对话框） -->
    <!-- 剪贴板预览确认 -->
    <div v-if="clipboardOpen" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="clipboardOpen = false">
      <div class="w-full max-w-md rounded-2xl border bg-card p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2 text-base font-semibold">
          <CloudUpload class="size-5 text-emerald-600" />
          发送剪贴板内容
        </div>
        <p class="mb-3 text-sm text-muted-foreground">发送给 {{ peerName }}</p>
        <textarea
          v-if="clipboardFallback"
          v-model="clipboardText"
          class="custom-scroll min-h-28 max-h-48 w-full rounded-xl border bg-background p-3 font-mono text-xs outline-none focus-visible:border-ring"
          placeholder="Ctrl/⌘ + V 粘贴要发送的内容…"
        ></textarea>
        <pre v-else class="custom-scroll max-h-48 overflow-auto rounded-xl border bg-muted/30 p-3 font-mono text-xs break-all whitespace-pre-wrap">{{ clipboardText }}</pre>
        <div class="mt-4 flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="clipboardOpen = false">取消</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700" @click="confirmSendClipboard">
            确认发送
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
