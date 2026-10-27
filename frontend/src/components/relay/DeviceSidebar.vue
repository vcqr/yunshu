<script setup lang="ts">
/**
 * DeviceSidebar：桌面软件式紧凑设备列表
 * 行式条目（h-9）+ 右键上下文菜单（打开会话 / 发送文件 / 发送文件夹 / 断开）
 * 手机（扫码配对）为云书特有的伪设备，与广播会话、飞鸽电脑并列
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  FileUp, FolderUp, Laptop, Lock, MessagesSquare, Radio, Satellite, Smartphone,
} from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import { store, deviceHasActiveTransfer } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { cn } from '../../lib/utils'

const emit = defineEmits<{ (e: 'select', peer: string): void }>()

const phoneOnline = computed(() => store.phoneConnected)
const phoneName = computed(() => (store.phoneConnected ? store.phoneName : '手机'))
const others = computed(() => store.devices)
const activeFor = (id: string) => deviceHasActiveTransfer(id)

/* ------------------------------ 右键菜单 ------------------------------ */

interface CtxDevice {
  id: string
  name: string
  ip: string
  online: boolean
}
const ctx = ref<{ x: number; y: number; device: CtxDevice } | null>(null)
/** 手机伪设备右键 */
const ctxPhone = ref<{ x: number; y: number } | null>(null)

function clampPos(x: number, y: number) {
  return {
    x: Math.max(4, Math.min(x, window.innerWidth - 190)),
    y: Math.max(4, Math.min(y, window.innerHeight - 180)),
  }
}

function openCtx(e: MouseEvent, device: CtxDevice) {
  ctx.value = { ...clampPos(e.clientX, e.clientY), device }
  ctxPhone.value = null
}
function openCtxPhone(e: MouseEvent) {
  ctxPhone.value = clampPos(e.clientX, e.clientY)
  ctx.value = null
}
function closeCtx() { ctx.value = null }
function closeCtxPhone() { ctxPhone.value = null }
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') { closeCtx(); closeCtxPhone() }
}
onMounted(() => window.addEventListener('keydown', onKey))
onUnmounted(() => window.removeEventListener('keydown', onKey))

async function ctxSelect() {
  const d = ctx.value?.device
  closeCtx()
  if (d) emit('select', d.id)
}
async function ctxSendFiles() {
  const d = ctx.value?.device
  closeCtx()
  if (!d) return
  emit('select', d.id)
  const { open } = await import('@tauri-apps/plugin-dialog')
  const paths = await open({ multiple: true, title: `选择要发送给「${d.name}」的文件` })
  if (!paths?.length) return
  relayActions.sendPaths('pc', d.id, paths)
}
async function ctxSendFolder() {
  const d = ctx.value?.device
  closeCtx()
  if (!d) return
  emit('select', d.id)
  const { open } = await import('@tauri-apps/plugin-dialog')
  const dir = await open({ directory: true, title: `选择要发送给「${d.name}」的文件夹（将自动打包为 zip）` })
  if (!dir) return
  relayActions.sendPaths('pc', d.id, [dir])
}
function ctxDisconnect() {
  const d = ctx.value?.device
  closeCtx()
  if (!d) return
  relayActions.disconnectPeer(d.id)
  toast.info(`已断开与「${d.name}」的连接`)
}

/* 手机右键：发送文件/文件夹（需已配对） */
async function ctxSendFilesPhone() {
  closeCtxPhone()
  if (!store.phoneConnected) { toast.error('手机未配对，请先扫码'); return }
  emit('select', 'phone')
  const { open } = await import('@tauri-apps/plugin-dialog')
  const paths = await open({ multiple: true, title: '选择要发送到手机的文件' })
  if (!paths?.length) return
  relayActions.sendPaths('phone', null, paths)
}
async function ctxSendFolderPhone() {
  closeCtxPhone()
  if (!store.phoneConnected) { toast.error('手机未配对，请先扫码'); return }
  emit('select', 'phone')
  const { open } = await import('@tauri-apps/plugin-dialog')
  const dir = await open({ directory: true, title: '选择要发送到手机的文件夹（将自动打包为 zip）' })
  if (!dir) return
  relayActions.sendPaths('phone', null, [dir])
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-1.5 p-2">
    <!-- 我的信息行 -->
    <div
      class="flex h-10 items-center gap-2.5 rounded-lg px-2 md:h-9"
      :title="`${store.meName} · ${store.meIp || store.serverAddr}`"
    >
      <RelayAvatar :name="store.meName" size="sm" :online="true" />
      <div class="min-w-0 flex-1 leading-tight">
        <p class="truncate text-[13px] font-medium" :title="store.meName">
          {{ store.meName }}
          <span class="ml-1 text-[10px] font-normal text-muted-foreground">（我）</span>
        </p>
      </div>
      <span
        v-if="store.meIp"
        class="shrink-0 font-mono text-[10px] text-muted-foreground"
      >{{ store.meIp }}</span>
    </div>

    <!-- 会话 -->
    <div class="shrink-0">
      <p class="px-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground">会话</p>
      <button
        type="button"
        role="tab"
        :aria-selected="store.selectedPeer === 'broadcast'"
        :class="cn(
          'flex h-10 w-full items-center gap-2.5 rounded-lg px-2 text-left transition-colors md:h-9',
          'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
          store.selectedPeer === 'broadcast'
            ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
            : 'hover:bg-accent'
        )"
        @click="emit('select', 'broadcast')"
      >
        <span
          :class="cn(
            'flex size-7 shrink-0 items-center justify-center rounded-lg',
            store.selectedPeer === 'broadcast'
              ? 'bg-gradient-to-br from-emerald-400 to-teal-600 text-white'
              : 'bg-muted text-muted-foreground'
          )"
          aria-hidden="true"
        >
          <Radio class="size-3.5" />
        </span>
        <span class="min-w-0 flex-1 truncate text-[13px] font-medium">所有人</span>
        <span class="shrink-0 text-[10px] text-muted-foreground">广播</span>
      </button>
    </div>

    <!-- 手机（扫码配对） -->
    <div class="shrink-0">
      <p class="px-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground">手机</p>
      <button
        type="button"
        role="tab"
        :aria-selected="store.selectedPeer === 'phone'"
        :title="phoneOnline ? `${phoneName} · 已配对` : '扫码配对手机'"
        :class="cn(
          'flex h-10 w-full items-center gap-2.5 rounded-lg px-2 text-left transition-colors md:h-9',
          'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
          store.selectedPeer === 'phone'
            ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
            : 'hover:bg-accent'
        )"
        @click="emit('select', 'phone')"
        @contextmenu.prevent="openCtxPhone"
      >
        <span
          :class="cn(
            'flex size-7 shrink-0 items-center justify-center rounded-lg',
            phoneOnline
              ? 'bg-gradient-to-br from-emerald-400 to-teal-600 text-white'
              : 'bg-muted text-muted-foreground'
          )"
          aria-hidden="true"
        >
          <Smartphone class="size-3.5" />
        </span>
        <span class="min-w-0 flex-1 truncate text-[13px] font-medium">{{ phoneName }}</span>
        <span
          v-if="activeFor('phone')"
          role="status"
          aria-label="有传输进行中"
          class="size-1.5 shrink-0 animate-pulse rounded-full bg-emerald-500"
        ></span>
        <span v-else class="shrink-0 text-[10px] text-muted-foreground">
          {{ phoneOnline ? '已配对' : '扫码配对' }}
        </span>
      </button>
    </div>

    <!-- 在线设备 -->
    <div class="flex min-h-0 flex-1 flex-col">
      <p class="shrink-0 px-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground">
        在线设备{{ others.length > 0 ? ` · ${others.length}` : '' }}
      </p>
      <div class="custom-scroll min-h-0 flex-1 overflow-y-auto">
        <div class="space-y-0.5 pb-2">
          <div
            v-if="others.length === 0"
            class="flex flex-col items-center gap-2.5 rounded-lg border border-dashed px-4 py-8 text-center"
          >
            <div class="flex items-center justify-center" aria-hidden="true">
              <Satellite class="size-7 text-emerald-500/70" stroke-width="1.6" />
              <Laptop class="-ml-1 size-5 text-muted-foreground/60" stroke-width="1.8" />
            </div>
            <div class="space-y-0.5">
              <p class="text-xs font-medium text-foreground">等待其他设备上线…</p>
              <p class="max-w-[190px] text-[10px] leading-relaxed text-muted-foreground">
                同一局域网内运行云书的电脑会自动出现在这里
              </p>
            </div>
          </div>
          <button
            v-for="d in others"
            :key="d.id"
            type="button"
            role="tab"
            :aria-selected="store.selectedPeer === d.id"
            :class="cn(
              'flex h-10 w-full items-center gap-2.5 rounded-lg px-2 text-left transition-colors md:h-9',
              'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
              store.selectedPeer === d.id
                ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
                : 'hover:bg-accent'
            )"
            @click="emit('select', d.id)"
            @contextmenu.prevent="openCtx($event, d)"
          >
            <RelayAvatar :name="d.name" size="sm" :online="d.online" />
            <span class="min-w-0 flex-1 truncate text-[13px] font-medium" :title="d.name">
              {{ d.name }}
            </span>
            <Lock
              v-if="store.fingerprints[d.id]"
              class="size-2.5 shrink-0 text-muted-foreground/70"
              aria-label="指纹已固定"
            />
            <span
              v-if="activeFor(d.id)"
              role="status"
              aria-label="有传输进行中"
              class="size-1.5 shrink-0 animate-pulse rounded-full bg-emerald-500"
            ></span>
            <span v-else class="shrink-0 font-mono text-[10px] text-muted-foreground">
              {{ d.ip }}
            </span>
          </button>
        </div>
      </div>
    </div>

    <!-- 电脑右键菜单 -->
    <Teleport to="body">
      <div
        v-if="ctx"
        class="fixed inset-0 z-[70]"
        @click="closeCtx"
        @contextmenu.prevent="closeCtx"
      >
        <div
          class="bg-popover absolute overflow-hidden rounded-lg border p-1 text-xs shadow-lg"
          :style="{ left: ctx.x + 'px', top: ctx.y + 'px' }"
        >
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="ctxSelect">
            <MessagesSquare class="size-3.5 text-emerald-600 dark:text-emerald-400" />
            打开会话
          </button>
          <div class="bg-border my-1 h-px"></div>
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="ctxSendFiles">
            <FileUp class="size-3.5 text-emerald-600 dark:text-emerald-400" />
            发送文件…
          </button>
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="ctxSendFolder">
            <FolderUp class="size-3.5 text-amber-500" />
            发送文件夹…
          </button>
          <template v-if="ctx.device.online">
            <div class="bg-border my-1 h-px"></div>
            <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 text-red-600 hover:bg-red-500/10 dark:text-red-400" @click="ctxDisconnect">
              <Lock class="size-3.5" />
              断开连接
            </button>
          </template>
        </div>
      </div>

      <!-- 手机右键菜单 -->
      <div
        v-if="ctxPhone"
        class="fixed inset-0 z-[70]"
        @click="closeCtxPhone"
        @contextmenu.prevent="closeCtxPhone"
      >
        <div
          class="bg-popover absolute overflow-hidden rounded-lg border p-1 text-xs shadow-lg"
          :style="{ left: ctxPhone.x + 'px', top: ctxPhone.y + 'px' }"
        >
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="closeCtxPhone(); emit('select', 'phone')">
            <MessagesSquare class="size-3.5 text-emerald-600 dark:text-emerald-400" />
            打开会话
          </button>
          <div class="bg-border my-1 h-px"></div>
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="ctxSendFilesPhone">
            <FileUp class="size-3.5 text-emerald-600 dark:text-emerald-400" />
            发送文件…
          </button>
          <button class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent" @click="ctxSendFolderPhone">
            <FolderUp class="size-3.5 text-amber-500" />
            发送文件夹…
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>
