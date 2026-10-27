<script setup lang="ts">
/**
 * Composer：消息输入区 — textarea 自适应、附件菜单、剪贴板发送
 * 桌面习惯：Ctrl+V 粘贴文件直接发起传输（经 stage_blob 落盘后走 send_paths）
 */
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { ClipboardPaste, FileUp, FolderUp, Plus, Send, UploadCloud } from '@lucide/vue'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { store } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

const props = defineProps<{ peer: string; peerName: string }>()

const text = ref('')
const menuOpen = ref(false)
const clipboardOpen = ref(false)
const clipboardText = ref('')
const clipboardFallback = ref(false)

const isPhone = computed(() => props.peer === 'phone')
const isBroadcast = computed(() => props.peer === 'broadcast')
const isTauri = '__TAURI_INTERNALS__' in window

function handleSend() {
  const content = text.value.trim()
  if (!content) return
  const ok = isBroadcast.value
    ? relayActions.broadcastText(content)
    : relayActions.sendText(props.peer, content)
  if (ok) text.value = ''
}

/* 文件 / 文件夹：Tauri 对话框取路径，打包与推送由 core 完成 */
async function pickFiles() {
  menuOpen.value = false
  if (isBroadcast.value) { toast.error('文件传输需要选择一个具体设备，请先在左侧选择对方设备'); return }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const paths = await open({ multiple: true, title: '选择要发送的文件' })
  if (!paths?.length) return
  relayActions.sendPaths(isPhone.value ? 'phone' : 'pc', isPhone.value ? null : props.peer, paths)
}
async function pickFolder() {
  menuOpen.value = false
  if (isBroadcast.value) { toast.error('文件传输需要选择一个具体设备，请先在左侧选择对方设备'); return }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const dir = await open({ directory: true, title: '选择要发送的文件夹（将自动打包为 zip）' })
  if (!dir) return
  relayActions.sendPaths(isPhone.value ? 'phone' : 'pc', isPhone.value ? null : props.peer, [dir])
}

/** 粘贴的文件浏览器侧拿不到路径：落到临时目录后再按路径发送 */
async function offerPastedFiles(files: File[]) {
  if (isBroadcast.value) {
    toast.error('文件传输需要选择一个具体设备，请先在左侧选择对方设备')
    return
  }
  if (!isTauri) {
    toast.error('当前环境不支持粘贴文件，请使用「发送文件」')
    return
  }
  try {
    const paths: string[] = []
    for (const f of files) {
      const bytes = new Uint8Array(await f.arrayBuffer())
      const path = await invoke<string>('stage_blob', { name: f.name || 'pasted.bin', bytes })
      paths.push(path)
    }
    relayActions.sendPaths(isPhone.value ? 'phone' : 'pc', isPhone.value ? null : props.peer, paths)
  } catch (e) {
    toast.error(`粘贴文件失败：${String(e)}`)
  }
}

async function onPaste(e: ClipboardEvent) {
  const files = Array.from(e.clipboardData?.files ?? [])
  if (files.length === 0) return // 文本粘贴走默认行为
  e.preventDefault()
  await offerPastedFiles(files)
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
</script>

<template>
  <div class="shrink-0 border-t bg-card/60 p-2 backdrop-blur-sm sm:p-2.5">
    <div class="flex items-end gap-1.5 sm:gap-2">
      <!-- 附件菜单 -->
      <div class="relative">
        <button
          aria-label="添加附件"
          title="发送文件 / 文件夹 / 剪贴板"
          :class="cn(
            'bg-background text-muted-foreground hover:text-foreground flex size-10 shrink-0 items-center justify-center rounded-lg border transition-colors hover:bg-accent md:size-9',
            menuOpen && 'border-ring ring-ring/50 ring-[3px]'
          )"
          @click.stop="menuOpen = !menuOpen"
        >
          <Plus class="size-4" />
        </button>
        <div
          v-if="menuOpen"
          class="bg-popover absolute bottom-12 left-0 z-30 w-52 overflow-hidden rounded-lg border p-1 shadow-lg"
          @click.stop
        >
          <button class="flex min-h-11 w-full items-center gap-2.5 rounded-md px-2.5 text-sm hover:bg-accent" @click="pickFiles">
            <FileUp class="size-4 text-emerald-600 dark:text-emerald-400" />
            发送文件
          </button>
          <button class="flex min-h-11 w-full items-center gap-2.5 rounded-md px-2.5 text-sm hover:bg-accent" @click="pickFolder">
            <FolderUp class="size-4 text-amber-500" />
            发送文件夹
          </button>
          <button class="flex min-h-11 w-full items-center gap-2.5 rounded-md px-2.5 text-sm hover:bg-accent" @click="openClipboardDialog">
            <ClipboardPaste class="size-4 text-violet-500" />
            发送剪贴板内容
          </button>
        </div>
      </div>

      <!-- 输入框 -->
      <textarea
        v-model="text"
        rows="1"
        :placeholder="isBroadcast
          ? '广播消息，Enter 发送…'
          : `发给 ${peerName}，Enter 发送…（Ctrl+V 可粘贴文件）`"
        aria-label="消息输入框"
        title="Enter 发送 · Shift+Enter 换行 · Ctrl+V 粘贴文件发送"
        :class="cn(
          'custom-scroll field-sizing-content max-h-28 min-h-10 w-full resize-none rounded-lg border bg-background px-3 py-2 text-[13px] shadow-xs',
          'transition-[color,box-shadow] outline-none placeholder:text-muted-foreground',
          'focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50'
        )"
        @paste="onPaste"
        @keydown.enter.exact.prevent="handleSend"
      ></textarea>

      <!-- 发送按钮 -->
      <button
        aria-label="发送消息"
        :disabled="!text.trim()"
        :class="cn(
          'flex size-10 shrink-0 items-center justify-center rounded-lg transition-transform md:size-9',
          text.trim()
            ? 'bg-gradient-to-r from-emerald-500 to-emerald-600 text-white shadow-sm shadow-emerald-500/30 hover:scale-105 hover:from-emerald-600 hover:to-emerald-700 active:scale-95'
            : 'bg-muted text-muted-foreground cursor-not-allowed'
        )"
        @click="handleSend"
      >
        <Send class="size-4" />
      </button>
    </div>

    <!-- 剪贴板预览确认 -->
    <div v-if="clipboardOpen" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="clipboardOpen = false">
      <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2 text-base font-semibold">
          <UploadCloud class="size-5 text-emerald-600 dark:text-emerald-400" />
          发送剪贴板内容
        </div>
        <p class="mb-3 text-sm text-muted-foreground">
          发送给{{ isBroadcast ? '所有在线设备（广播）' : peerName }}
        </p>
        <textarea
          v-if="clipboardFallback"
          v-model="clipboardText"
          class="custom-scroll min-h-28 max-h-48 w-full rounded-xl border bg-background p-3 font-mono text-xs outline-none focus-visible:border-ring"
          placeholder="Ctrl/⌘ + V 粘贴要发送的内容…"
          aria-label="剪贴板内容输入"
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
