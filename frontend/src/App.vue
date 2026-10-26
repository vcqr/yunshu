<script setup lang="ts">
/**
 * 云书 — 主页面（移植自 demo page.tsx）
 * 布局：Header / Sidebar / ChatPanel / TransferPanel / Footer
 */
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { ShieldCheck, Send, Zap, Settings2 } from '@lucide/vue'
import AppHeader from './components/relay/AppHeader.vue'
import DeviceSidebar from './components/relay/DeviceSidebar.vue'
import ChatPanel from './components/relay/ChatPanel.vue'
import TransferPanel from './components/relay/TransferPanel.vue'
import OfferDialog from './components/relay/OfferDialog.vue'
import { startRelay, relayActions } from './hooks/use-relay'
import { store, splitTransfers } from './stores/relay-store'
import { toastState } from './hooks/use-toast'
import { cn } from './lib/utils'

const currentTrust = computed(() => store.trustRequests[0] ?? null)
function trustDevice() {
  if (!currentTrust.value) return
  invoke('pigeon_trust', {
    peerId: currentTrust.value.peer_id,
    fingerprint: currentTrust.value.fingerprint,
  })
  store.trustRequests = store.trustRequests.filter((r) => r.peer_id !== currentTrust.value.peer_id)
}
function rejectTrust() {
  if (!currentTrust.value) return
  invoke('pigeon_reject_trust', { peerId: currentTrust.value.peer_id })
  store.trustRequests = store.trustRequests.filter((r) => r.peer_id !== currentTrust.value.peer_id)
}
const showSettings = ref(false)
const settingsForm = ref({
  device_name: '', port: 8000, save_dir: '', autostart: false,
})
const filterDone = ref(false)

const activeCount = computed(() => splitTransfers().active.length)

function openSettings() {
  window.__yunshuGetSettings?.().then((s) => {
    settingsForm.value = s
    showSettings.value = true
  })
}
function saveSettings() {
  window.__yunshuSaveSettings?.(settingsForm.value).then((r: string | null) => {
    if (r) toastState.items.push({ id: Date.now(), kind: 'error', text: `保存失败：${r}` })
    else {
      toastState.items.push({ id: Date.now(), kind: 'success', text: '设置已保存（端口、目录重启后生效）' })
      showSettings.value = false
    }
  })
}

function onSelectPeer(id: string) {
  store.selectedPeer = id
}

function onDrop(e: CustomEvent) {
  const paths: string[] = e.detail
  if (!store.selectedPeer) {
    toastState.items.push({ id: Date.now(), kind: 'error', text: '先在左侧选择一台设备' })
    return
  }
  const kind = store.selectedPeer === 'phone' ? 'phone' : 'pc'
  const peerId = kind === 'pc' ? store.selectedPeer : null
  if (kind === 'phone' && !store.phoneConnected) {
    toastState.items.push({ id: Date.now(), kind: 'error', text: '手机未配对，请先扫码' })
    return
  }
  relayActions.sendPaths(kind as 'phone' | 'pc', peerId, paths)
}

onMounted(async () => {
  await startRelay()
  window.addEventListener('yunshu-drop', (e) => onDrop(e as CustomEvent))
  window.__yunshuGetSettings = () => import('@tauri-apps/api/core').then((c) => c.invoke('get_settings'))
  window.__yunshuSaveSettings = (s: any) =>
    import('@tauri-apps/api/core').then((c) =>
      c.invoke('save_settings', {
        deviceName: s.device_name, port: s.port, saveDir: s.save_dir, autostart: s.autostart,
      }).then(() => null).catch((e: any) => String(e))
    )
})
</script>

<template>
  <div class="airpigeon-bg flex min-h-dvh flex-col">
    <AppHeader @open-settings="openSettings" />

    <main class="flex min-h-0 flex-1 overflow-hidden">
      <aside class="w-72 shrink-0 border-r bg-card/50" aria-label="设备侧栏">
        <DeviceSidebar @select="onSelectPeer" />
      </aside>

      <ChatPanel v-if="store.selectedPeer" :peer="store.selectedPeer" />
      <div v-else class="flex min-w-0 flex-1 items-center justify-center">
        <div class="flex flex-col items-center gap-4 text-center">
          <div class="flex size-20 items-center justify-center rounded-3xl bg-gradient-to-br from-emerald-500/15 to-teal-500/10 text-emerald-600">
            <Send class="size-9" />
          </div>
          <div class="space-y-1.5">
            <p class="font-medium">选择左侧设备开始聊天与传文件</p>
            <p class="text-xs text-muted-foreground">手机扫码配对 · 电脑自动发现 · 全程局域网加密</p>
          </div>
        </div>
      </div>

      <aside class="w-80 shrink-0 border-l bg-card/50" aria-label="传输面板">
        <TransferPanel v-model:filter-done="filterDone" />
      </aside>
    </main>

    <footer class="mt-auto border-t bg-background/80 backdrop-blur-sm">
      <div class="flex flex-wrap items-center justify-center gap-x-4 gap-y-1 px-4 py-2.5 text-[11px] text-muted-foreground">
        <span class="flex items-center gap-1">
          <Send class="size-3 text-emerald-500" aria-hidden="true" />
          © 云书 · 局域网传书
        </span>
        <span class="flex items-center gap-1">
          <ShieldCheck class="size-3" aria-hidden="true" />
          局域网点对点安全传输
        </span>
        <span class="hidden items-center gap-1 sm:flex">
          <Zap class="size-3" aria-hidden="true" />
          数据不经公网中转
        </span>
      </div>
    </footer>

    <!-- 接收请求队列弹窗 -->
    <OfferDialog />

    <!-- 新设备信任确认（TOFU） -->
    <div v-if="currentTrust" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="rejectTrust">
      <div class="w-full max-w-md rounded-2xl border bg-card p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2.5 text-base font-semibold">
          🔒 新设备待确认
        </div>
        <p class="mb-1 text-sm">「{{ currentTrust.name }}」想要接入本机</p>
        <p class="mb-3 text-xs text-muted-foreground">请核对下方指纹与对方屏幕显示一致后，再点击信任</p>
        <div class="mb-4 rounded-xl border bg-muted/30 p-3 font-mono text-xs leading-relaxed break-all text-foreground/80">
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
      <div class="w-full max-w-md rounded-2xl border bg-card p-5 shadow-xl">
        <div class="mb-3 flex items-center gap-2 text-base font-semibold">
          <Settings2 class="size-5 text-muted-foreground" />
          设置
        </div>
        <div class="mb-2 flex items-center gap-3">
          <span class="w-16 shrink-0 text-sm text-muted-foreground">电脑名称</span>
          <input v-model="settingsForm.device_name" class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-none focus-visible:border-ring" />
        </div>
        <div class="mb-2 flex items-center gap-3">
          <span class="w-16 shrink-0 text-sm text-muted-foreground">端口</span>
          <input v-model.number="settingsForm.port" type="number" class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-none focus-visible:border-ring" />
        </div>
        <div class="mb-2 flex items-center gap-3">
          <span class="w-16 shrink-0 text-sm text-muted-foreground">保存目录</span>
          <input v-model="settingsForm.save_dir" class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-none focus-visible:border-ring" />
        </div>
        <label class="mb-3 flex items-center gap-3 text-sm">
          <span class="w-16 shrink-0 text-muted-foreground">开机自启</span>
          <input v-model="settingsForm.autostart" type="checkbox" />
        </label>
        <p class="text-xs text-muted-foreground">端口、目录修改后需重启云书生效</p>
        <div class="mt-4 flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="showSettings = false">关闭</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700" @click="saveSettings">保存</button>
        </div>
      </div>
    </div>

    <!-- Toast -->
    <div class="fixed bottom-12 right-4 z-[60] flex flex-col gap-1.5">
      <div
        v-for="t in toastState.items"
        :key="t.id"
        :class="cn(
          'rounded-xl px-4 py-2.5 text-sm shadow-lg backdrop-blur',
          t.kind === 'error' ? 'bg-red-600/95 text-white' : t.kind === 'success' ? 'bg-emerald-600/95 text-white' : 'bg-slate-800/95 text-white'
        )"
      >
        {{ t.text }}
      </div>
    </div>
  </div>
</template>

<style>
.airpigeon-bg {
  background:
    radial-gradient(1200px 500px at 80% -10%, rgba(16, 185, 129, 0.06), transparent),
    var(--color-air-bg);
}
</style>
