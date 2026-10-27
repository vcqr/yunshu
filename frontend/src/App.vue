<script setup lang="ts">
/**
 * 云书 — 主页面（复刻 demo/ui page.tsx）
 * 桌面软件式布局：紧凑标题栏 / 设备侧栏(md+) / 聊天主区 / 传输面板(lg+ 第三栏) / 底部状态栏
 * 云书已有功能：手机扫码配对（会话内视图）、TOFU 信任确认、设置、打开所在文件夹
 */
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { Bird, ShieldCheck } from '@lucide/vue'
import AppHeader from './components/relay/AppHeader.vue'
import DeviceSidebar from './components/relay/DeviceSidebar.vue'
import ChatPanel from './components/relay/ChatPanel.vue'
import TransferPanel from './components/relay/TransferPanel.vue'
import OfferDialog from './components/relay/OfferDialog.vue'
import StatusBar from './components/relay/StatusBar.vue'
import { startRelay } from './hooks/use-relay'
import { store, mutations, toggleTransferPanelPersist, restoreTransferPanelPref, restoreChatStylePref } from './stores/relay-store'
import { toast, toastState } from './hooks/use-toast'
import { cn } from './lib/utils'
import { installTauriDblclickGuard, initWindowControls } from './lib/window-controls'

/* 移动端/窄窗抽屉 */
const sidebarOpen = ref(false)

/* 拖拽发送（Tauri drag-drop 落在本窗口的任意位置都发到当前会话） */
function onDrop(e: Event) {
  const paths: string[] = (e as CustomEvent).detail
  const peer = store.selectedPeer
  if (!peer || peer === 'broadcast') {
    toast.error('先在左侧选择一台设备')
    return
  }
  const kind = peer === 'phone' ? 'phone' : 'pc'
  relayActions.sendPaths(kind, kind === 'pc' ? peer : null, paths)
}

/* ------------------------------ TOFU 信任确认 ------------------------------ */
const currentTrust = computed(() => store.trustRequests[0] ?? null)
function trustDevice() {
  if (!currentTrust.value) return
  invoke('pigeon_trust', {
    peerId: currentTrust.value.peer_id,
    fingerprint: currentTrust.value.fingerprint,
  })
  store.trustRequests = store.trustRequests.filter((r) => r.peer_id !== currentTrust.value!.peer_id)
}
function rejectTrust() {
  if (!currentTrust.value) return
  invoke('pigeon_reject_trust', { peerId: currentTrust.value.peer_id })
  store.trustRequests = store.trustRequests.filter((r) => r.peer_id !== currentTrust.value!.peer_id)
}

/* ------------------------------ 设置 ------------------------------ */
const showSettings = ref(false)
const settingsForm = ref({ device_name: '', port: 34256, save_dir: '', autostart: false, dnd_mode: false })
async function openSettings() {
  const s = await invoke<any>('get_settings').catch(() => null)
  if (s) settingsForm.value = { ...s }
  showSettings.value = true
}
async function browseSaveDir() {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const dir = await open({ directory: true, title: '选择接收文件的保存目录' })
  if (dir) settingsForm.value.save_dir = dir as string
}
async function saveSettings() {
  const s = settingsForm.value
  try {
    await invoke('save_settings', {
      deviceName: s.device_name, port: s.port, saveDir: s.save_dir, autostart: s.autostart, dndMode: s.dnd_mode,
    })
    store.meName = s.device_name
    store.dnd = s.dnd_mode
    toast.success('设置已保存（端口、目录重启后生效）')
    showSettings.value = false
  } catch (e) {
    toast.error(`保存失败：${String(e)}`)
  }
}

/* ------------------------------ 生命周期 ------------------------------ */
onMounted(async () => {
  restoreTransferPanelPref()
  restoreChatStylePref()
  // 无边框窗口：标题栏双击/三击守卫 + 窗口状态跟随（最大化/全屏图标切换）
  installTauriDblclickGuard()
  initWindowControls()
  await startRelay()
  window.addEventListener('yunshu-drop', onDrop)
})
</script>

<template>
  <div class="airpigeon-bg flex h-dvh flex-col overflow-hidden">
    <AppHeader
      @open-settings="openSettings"
      @toggle-sidebar="sidebarOpen = true"
    />

    <main class="flex min-h-0 flex-1 overflow-hidden">
      <!-- 侧栏：md+ 常驻 -->
      <aside
        class="hidden w-60 shrink-0 border-r bg-card/40 md:block"
        aria-label="设备侧栏"
      >
        <DeviceSidebar
          @select="(p: string) => mutations.setSelectedPeer(p)"
        />
      </aside>

      <!-- 聊天主区（广播/手机/点对点统一由 ChatPanel 渲染） -->
      <ChatPanel :peer="store.selectedPeer" />

      <!-- 第三栏：传输任务面板（lg+ 常驻，可收起/展开） -->
      <Transition name="panel">
        <aside
          v-if="store.transferPanelOpen"
          class="hidden w-80 shrink-0 overflow-hidden border-l bg-card/40 lg:block"
          aria-label="传输任务面板"
        >
          <TransferPanel :on-collapse="toggleTransferPanelPersist" />
        </aside>
      </Transition>
    </main>

    <!-- 桌面式状态栏（始终贴底） -->
    <StatusBar />

    <!-- 窄窗设备抽屉 -->
    <Teleport to="body">
      <div v-if="sidebarOpen" class="fixed inset-0 z-50 md:hidden" @click.self="sidebarOpen = false">
        <div class="bg-background absolute inset-y-0 left-0 w-72 border-r pt-12 shadow-xl">
          <p class="sr-only">设备与会话</p>
          <div class="h-full min-h-0">
            <DeviceSidebar @select="(p: string) => { mutations.setSelectedPeer(p); sidebarOpen = false }" />
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 接收请求队列弹窗 -->
    <OfferDialog />

    <!-- 新设备信任确认（TOFU） -->
    <div v-if="currentTrust" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="rejectTrust">
      <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2.5 text-base font-semibold">
          <ShieldCheck class="size-5 text-emerald-600" />
          新设备待确认
        </div>
        <p class="mb-1 text-sm">「{{ currentTrust.name }}」想要接入本机</p>
        <p class="mb-3 text-xs text-muted-foreground">请核对下方指纹与对方屏幕显示一致后，再点击信任</p>
        <div class="mb-4 break-all rounded-xl border bg-muted/30 p-3 font-mono text-xs leading-relaxed text-foreground/80">
          {{ currentTrust.fingerprint }}
        </div>
        <div class="flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="rejectTrust">拒绝</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700" @click="trustDevice">
            信任此设备
          </button>
        </div>
      </div>
    </div>

    <!-- 设置 -->
    <div v-if="showSettings" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="showSettings = false">
      <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
        <div class="mb-4 flex items-center gap-2.5">
          <div class="flex size-9 items-center justify-center rounded-xl bg-gradient-to-br from-emerald-400 via-emerald-500 to-teal-600 text-white">
            <Bird class="size-5" />
          </div>
          <div>
            <p class="text-sm font-semibold">设置</p>
            <p class="text-xs text-muted-foreground">设备名修改后立即对局域网内其他设备生效</p>
          </div>
        </div>
        <div class="grid gap-2.5">
          <label class="grid gap-1">
            <span class="text-xs font-medium text-muted-foreground">设备名</span>
            <input
              v-model="settingsForm.device_name"
              maxlength="24"
              placeholder="给你的设备起个名字"
              class="h-9 rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
            />
          </label>
          <label class="grid gap-1">
            <span class="text-xs font-medium text-muted-foreground">端口</span>
            <input
              v-model.number="settingsForm.port"
              type="number"
              class="h-9 rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
            />
          </label>
          <label class="grid gap-1">
            <span class="text-xs font-medium text-muted-foreground">保存目录</span>
            <div class="flex gap-2">
              <input
                v-model="settingsForm.save_dir"
                class="h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              />
              <button class="h-9 shrink-0 rounded-lg border px-3 text-sm hover:bg-accent" @click="browseSaveDir">浏览…</button>
            </div>
          </label>
          <label class="flex items-center gap-2.5 text-sm">
            <input v-model="settingsForm.autostart" type="checkbox" class="size-4 accent-emerald-600" />
            开机自启
          </label>
          <label class="flex items-center gap-2.5 text-sm">
            <input v-model="settingsForm.dnd_mode" type="checkbox" class="size-4 accent-emerald-600" />
            防打扰（仅已信任设备可连接，广播消息静音）
          </label>
        </div>
        <p class="mt-3 text-xs text-muted-foreground">端口、目录修改后需重启云书生效</p>
        <div class="mt-4 flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="showSettings = false">取消</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700" @click="saveSettings">保存</button>
        </div>
      </div>
    </div>

    <!-- Toast -->
    <div class="fixed bottom-10 right-4 z-[60] flex flex-col items-end gap-1.5">
      <div
        v-for="t in toastState.items"
        :key="t.id"
        :class="cn(
          'max-w-sm rounded-xl px-4 py-2.5 text-sm shadow-lg backdrop-blur',
          t.kind === 'error' ? 'bg-red-600/95 text-white' : t.kind === 'success' ? 'bg-emerald-600/95 text-white' : 'bg-slate-800/95 text-white'
        )"
        role="status"
      >
        {{ t.text }}
      </div>
    </div>
  </div>
</template>

<style>
/* 第三栏宽度滑动动画（复刻 framer-motion width 0↔320, 0.22s） */
.panel-enter-active,
.panel-leave-active {
  transition: width 0.22s cubic-bezier(0.32, 0.72, 0, 1), opacity 0.22s ease;
  overflow: hidden;
}
.panel-enter-from,
.panel-leave-to {
  width: 0 !important;
  opacity: 0;
}
.panel-enter-to,
.panel-leave-from {
  width: 320px;
}
</style>
