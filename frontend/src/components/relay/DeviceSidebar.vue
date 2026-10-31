<script setup lang="ts">
/**
 * DeviceSidebar：桌面软件式紧凑设备列表
 * 行式条目（h-9）+ 右键上下文菜单（打开会话 / 发送文件 / 发送文件夹 / 断开）
 * 手机（扫码配对）为 Yunshu 特有的伪设备，与广播会话、飞鸽电脑并列
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  Check, FileUp, FolderUp, Laptop, Lock, MessageCircle, MessagesSquare, MonitorUp, HandMetal, Plus, Radio,
  RefreshCw, Satellite, Smartphone, Trash2,
} from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import UnreadBadge from './UnreadBadge.vue'
import { store, mutations, deviceHasActiveTransfer, unreadFor, myPresence } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { cn } from '../../lib/utils'

const emit = defineEmits<{ (e: 'select', peer: string): void }>()

const phoneOnline = computed(() => store.phoneConnected)
const phoneName = computed(() => (store.phoneConnected ? store.phoneName : '手机'))
const others = computed(() => store.devices)
const activeFor = (id: string) => deviceHasActiveTransfer(id)

/* ------------------------------ 本机状态菜单 ------------------------------ */
/* Slack 式：点头像弹状态菜单（在线/免打扰/隐身互斥一键切换），点行体回首页 */

type PresenceOption = {
  value: 'online' | 'dnd' | 'stealth'
  label: string
  desc: string
  dot: string
}

const presenceOptions: PresenceOption[] = [
  {
    value: 'online',
    label: '在线',
    desc: '正常广播本机并接受连接',
    dot: 'bg-emerald-500',
  },
  {
    value: 'dnd',
    label: '免打扰',
    desc: '仅已信任设备可连接，广播消息静音',
    dot: 'bg-violet-500',
  },
  {
    value: 'stealth',
    label: '隐身',
    desc: '不向局域网广播本机，他人列表立即移除',
    dot: 'bg-amber-400',
  },
]

const presenceMenu = ref<{ x: number; y: number } | null>(null)

function openPresenceMenu(e: MouseEvent) {
  const w = 252
  const h = 186
  presenceMenu.value = {
    x: Math.max(4, Math.min(e.clientX + 8, window.innerWidth - w - 4)),
    y: Math.max(4, Math.min(e.clientY + 12, window.innerHeight - h - 4)),
  }
}

function pickPresence(value: PresenceOption['value']) {
  presenceMenu.value = null
  // 三态互斥一键切换（精细组合可去设置里配）
  relayActions.setPresence(value === 'dnd', value === 'stealth')
}

/* ------------------------------ 在线设备搜索 ------------------------------ */
/* 按名称 / IP / 机器 ID 过滤（设备多时快速定位） */
const search = ref('')
const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  if (!q) return others.value
  return others.value.filter(
    (d) =>
      d.name.toLowerCase().includes(q) ||
      d.ip.toLowerCase().includes(q) ||
      (d.machineId || '').toLowerCase().includes(q),
  )
})

/* ------------------------------ 右键菜单 ------------------------------ */

interface CtxDevice {
  id: string
  name: string
  ip: string
  online: boolean
  manual?: boolean
  addr?: string
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
/** 右键发起远程控制（打开密码弹窗） */
function ctxRemoteControl() {
  const d = ctx.value?.device
  closeCtx()
  if (!d) return
  emit('select', d.id)
  relayActions.openControlDialog(d.machineId || '', d.name)
}
/** 右键请求对方协助（反向协助：对方同意后自动建连） */
function ctxAssistRequest() {
  const d = ctx.value?.device
  closeCtx()
  if (!d) return
  relayActions.assistRequest(d.id)
}
/** 右键重连/移除手动主机 */
function ctxReconnect() {
  const d = ctx.value?.device
  closeCtx()
  if (!d?.addr) return
  relayActions.connectHost(d.addr)
  toast.info(`正在连接「${d.name}」…`)
}
function ctxRemoveHost() {
  const d = ctx.value?.device
  closeCtx()
  if (!d?.addr) return
  if (d.id === store.selectedPeer) mutations.setSelectedPeer('self')
  relayActions.removeHost(d.addr)
}
/** 点击设备：离线的手动主机顺手发起重连 */
function onSelectDevice(d: CtxDevice) {
  emit('select', d.id)
  if (d.manual && !d.online && d.addr) relayActions.connectHost(d.addr)
}

/* ------------------------------ 自助添加主机 ------------------------------ */

const showAdd = ref(false)
const addForm = ref({ addr: '', name: '' })
function openAdd() {
  addForm.value = { addr: '', name: '' }
  showAdd.value = true
}
function submitAdd() {
  const addr = addForm.value.addr.trim()
  if (!addr) {
    toast.error('请输入对方地址（IP 或主机名，可带端口）')
    return
  }
  relayActions.addHost(addr, addForm.value.name.trim())
  toast.info(`正在连接 ${addr}…（首次连接需双方确认指纹）`)
  showAdd.value = false
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
    <!-- 我的信息行：头像钮弹状态菜单；行体点击回「本机识别」首页 -->
    <div
      class="group flex h-10 w-full items-center gap-2.5 rounded-lg px-2 transition-colors md:h-9"
      :class="
        store.selectedPeer === 'self'
          ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
          : 'hover:bg-accent'
      "
    >
      <button
        type="button"
        class="relative shrink-0 rounded-full outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
        title="更改本机状态"
        aria-label="更改本机状态"
        @click.stop="openPresenceMenu($event)"
      >
        <RelayAvatar :name="store.meName" size="sm" :presence="myPresence()" />
      </button>
      <button
        type="button"
        class="min-w-0 flex-1 text-left leading-tight outline-none focus-visible:rounded-lg focus-visible:ring-[3px] focus-visible:ring-ring/50"
        :title="`${store.meName} · 本机识别（机器 ID 与验证码）`"
        @click="emit('select', 'self')"
      >
        <p class="truncate text-[13px] font-medium" :title="store.meName">
          {{ store.meName }}
          <span class="ml-1 text-[10px] font-normal text-muted-foreground">（我）</span>
        </p>
      </button>
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
        <span
          class="min-w-0 flex-1 truncate text-[13px]"
          :class="unreadFor('broadcast') > 0 ? 'font-semibold' : 'font-medium'"
        >所有人</span>
        <UnreadBadge v-if="unreadFor('broadcast') > 0" :count="unreadFor('broadcast')" />
        <span v-else class="shrink-0 text-[10px] text-muted-foreground">广播</span>
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
        <span
          class="min-w-0 flex-1 truncate text-[13px]"
          :class="unreadFor('phone') > 0 ? 'font-semibold' : 'font-medium'"
        >{{ phoneName }}</span>
        <span
          v-if="activeFor('phone')"
          role="status"
          aria-label="有传输进行中"
          class="size-1.5 shrink-0 animate-pulse rounded-full bg-emerald-500"
        ></span>
        <UnreadBadge v-if="unreadFor('phone') > 0" :count="unreadFor('phone')" />
        <span
          v-else-if="!activeFor('phone')"
          class="shrink-0 text-[10px] text-muted-foreground"
        >{{ phoneOnline ? '已配对' : '扫码配对' }}</span>
      </button>

      <!-- 微信（个人微信机器人）：点击切到微信对话界面；隐藏开关关闭时整个入口不渲染 -->
      <button
        v-if="store.weixinEnabled"
        type="button"
        role="tab"
        :aria-selected="store.selectedPeer === 'weixin'"
        :title="store.weixinLoggedIn ? '微信机器人已连接' : '扫码绑定个人微信机器人'"
        :class="cn(
          'flex h-10 w-full items-center gap-2.5 rounded-lg px-2 text-left transition-colors md:h-9',
          'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
          store.selectedPeer === 'weixin'
            ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
            : 'hover:bg-accent'
        )"
        @click="emit('select', 'weixin')"
      >
        <span
          :class="cn(
            'flex size-7 shrink-0 items-center justify-center rounded-lg',
            store.weixinLoggedIn
              ? 'bg-gradient-to-br from-emerald-400 to-teal-600 text-white'
              : 'bg-muted text-muted-foreground'
          )"
          aria-hidden="true"
        >
          <MessageCircle class="size-3.5" />
        </span>
        <span
          class="min-w-0 flex-1 truncate text-[13px]"
          :class="unreadFor('weixin') > 0 ? 'font-semibold' : 'font-medium'"
        >微信</span>
        <UnreadBadge v-if="unreadFor('weixin') > 0" :count="unreadFor('weixin')" />
        <span
          v-else
          class="shrink-0 text-[10px] text-muted-foreground"
        >{{ store.weixinLoggedIn ? '已连接' : '未登录' }}</span>
      </button>
    </div>

    <!-- 在线设备 -->
    <div class="flex min-h-0 flex-1 flex-col">
      <div class="shrink-0 px-1 pb-1.5">
        <input
          v-model="search"
          placeholder="搜索名称 / IP / 机器 ID"
          aria-label="搜索在线设备"
          class="h-7 w-full rounded-lg border bg-background px-2.5 text-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
        />
      </div>
      <div class="flex shrink-0 items-center justify-between px-2 pb-1">
        <p class="text-[10px] font-medium tracking-wide text-muted-foreground">
          在线设备{{ filtered.length > 0 ? ` · ${filtered.length}` : '' }}
        </p>
        <button
          type="button"
          class="flex items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground hover:bg-accent hover:text-emerald-600"
          title="自助添加主机（对方不广播或跨网段时手动连入）"
          @click="openAdd"
        >
          <Plus class="size-3" />
          添加主机
        </button>
      </div>
      <div class="custom-scroll min-h-0 flex-1 overflow-y-auto">
        <div class="space-y-0.5 pb-2">
          <!-- 无在线设备：引导空态；有设备但搜索无匹配：提示换关键词 -->
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
                同一局域网内运行 Yunshu 的电脑会自动出现在这里
              </p>
            </div>
          </div>
          <p
            v-else-if="filtered.length === 0"
            class="py-6 text-center text-xs text-muted-foreground"
          >
            没有匹配「{{ search.trim() }}」的设备
          </p>
          <button
            v-for="d in filtered"
            :key="d.id"
            type="button"
            role="tab"
            :aria-selected="store.selectedPeer === d.id"
            :class="cn(
              'group flex h-10 w-full items-center gap-2.5 rounded-lg px-2 text-left transition-colors md:h-9',
              'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
              store.selectedPeer === d.id
                ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300'
                : 'hover:bg-accent'
            )"
            @click="onSelectDevice(d)"
            @contextmenu.prevent="openCtx($event, d)"
          >
            <RelayAvatar :name="d.name" size="sm" :presence="d.online ? 'online' : 'offline'" />
            <span
              class="min-w-0 flex-1 truncate text-[13px]"
              :class="unreadFor(d.id) > 0 ? 'font-semibold' : 'font-medium'"
              :title="d.name"
            >
              {{ d.name }}
            </span>
            <span
              v-if="d.manual && !d.online"
              class="shrink-0 text-[10px] text-muted-foreground"
              title="手动添加的主机，当前离线"
            >离线</span>
            <span
              v-if="d.machineId"
              role="button"
              tabindex="0"
              aria-label="远程控制"
              title="远程控制"
              class="hidden size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-emerald-600 group-hover:flex"
              @click.stop="relayActions.openControlDialog(d.machineId, d.name)"
              @keydown.enter.stop="relayActions.openControlDialog(d.machineId, d.name)"
            >
              <MonitorUp class="size-3.5" />
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
            <UnreadBadge v-if="unreadFor(d.id) > 0" :count="unreadFor(d.id)" />
            <span
              v-else-if="!activeFor(d.id)"
              class="shrink-0 font-mono text-[10px] text-muted-foreground"
            >
              {{ d.ip }}
            </span>
          </button>
        </div>
      </div>
    </div>

    <!-- 本机状态菜单（Teleport 到 body，点击外部关闭） -->
    <Teleport to="body">
      <div
        v-if="presenceMenu"
        class="fixed inset-0 z-[70]"
        @click="presenceMenu = null"
        @contextmenu.prevent="presenceMenu = null"
      >
        <div
          class="bg-popover absolute w-64 overflow-hidden rounded-xl border p-1.5 shadow-lg"
          :style="{ left: presenceMenu.x + 'px', top: presenceMenu.y + 'px' }"
          role="menu"
          aria-label="本机状态"
        >
          <p class="px-2.5 py-1.5 text-[10px] font-medium tracking-wide text-muted-foreground">
            本机状态
          </p>
          <button
            v-for="opt in presenceOptions"
            :key="opt.value"
            type="button"
            role="menuitemradio"
            :aria-checked="myPresence() === opt.value"
            class="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left transition-colors hover:bg-accent"
            @click="pickPresence(opt.value)"
          >
            <span
              :class="cn('size-2.5 shrink-0 rounded-full', opt.dot)"
              aria-hidden="true"
            />
            <span class="min-w-0 flex-1">
              <span class="block text-[13px] font-medium">{{ opt.label }}</span>
              <span class="block text-[10px] leading-snug text-muted-foreground">{{ opt.desc }}</span>
            </span>
            <Check
              v-if="myPresence() === opt.value"
              class="size-4 shrink-0 text-emerald-600 dark:text-emerald-400"
              aria-hidden="true"
            />
          </button>
        </div>
      </div>
    </Teleport>

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
          <button
            v-if="ctx.device.machineId"
            class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent"
            @click="ctxRemoteControl"
          >
            <MonitorUp class="size-3.5 text-emerald-600 dark:text-emerald-400" />
            远程控制…
          </button>
          <button
            v-if="ctx.device.online"
            class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent"
            @click="ctxAssistRequest"
          >
            <HandMetal class="size-3.5 text-amber-500" />
            请求协助控制对方
          </button>
          <template v-if="ctx.device.manual">
            <button
              v-if="!ctx.device.online"
              class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 hover:bg-accent"
              @click="ctxReconnect"
            >
              <RefreshCw class="size-3.5 text-emerald-600 dark:text-emerald-400" />
              重新连接
            </button>
            <button
              class="flex min-h-9 w-full items-center gap-2 rounded-md px-2.5 text-red-600 hover:bg-red-500/10 dark:text-red-400"
              @click="ctxRemoveHost"
            >
              <Trash2 class="size-3.5" />
              移除此主机
            </button>
          </template>
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
    <!-- 自助添加主机弹窗 -->
    <Teleport to="body">
      <div
        v-if="showAdd"
        class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45"
        @click.self="showAdd = false"
      >
        <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
          <div class="mb-1 flex items-center gap-2.5 text-base font-semibold">
            <Plus class="size-5 text-emerald-600" />
            添加主机
          </div>
          <p class="mb-4 text-xs text-muted-foreground">
            输入同一局域网（或可达网段）内运行 Yunshu 的电脑地址；双方首次连接需各自确认指纹
          </p>
          <label class="mb-1 block text-xs font-medium text-muted-foreground">对方地址</label>
          <input
            v-model="addForm.addr"
            placeholder="192.168.1.5 或 192.168.1.5:8001"
            class="mb-3 h-9 w-full rounded-lg border bg-background px-3 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
            @keydown.enter="submitAdd"
          />
          <label class="mb-1 block text-xs font-medium text-muted-foreground">备注名（可选）</label>
          <input
            v-model="addForm.name"
            maxlength="24"
            placeholder="留空则自动采用对方的电脑名"
            class="mb-4 h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
            @keydown.enter="submitAdd"
          />
          <p class="mb-4 text-[11px] leading-relaxed text-muted-foreground">
            提示：对方若开启了隐身模式，需由对方也添加本机（或由非隐身一方发起添加）才能互相建立连接
          </p>
          <div class="flex justify-end gap-2">
            <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="showAdd = false">取消</button>
            <button
              class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700"
              @click="submitAdd"
            >
              连接
            </button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
