<script setup lang="ts">
/**
 * Yunshu — 主页面（复刻 demo/ui page.tsx）
 * 桌面软件式布局：紧凑标题栏 / 设备侧栏(md+) / 聊天主区 / 传输面板(lg+ 第三栏) / 底部状态栏
 * Yunshu 已有功能：手机扫码配对（会话内视图）、TOFU 信任确认、设置、打开所在文件夹
 */
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { Globe, HandMetal, MessageCircle, MonitorUp, Settings2, ShieldAlert, ShieldCheck, Smartphone } from '@lucide/vue'
import AppHeader from './components/relay/AppHeader.vue'
import DeviceSidebar from './components/relay/DeviceSidebar.vue'
import LocalIdentityCard from './components/relay/LocalIdentityCard.vue'
import ChatPanel from './components/relay/ChatPanel.vue'
import WeixinPanel from './components/relay/WeixinPanel.vue'
import TransferPanel from './components/relay/TransferPanel.vue'
import OfferDialog from './components/relay/OfferDialog.vue'
import StatusBar from './components/relay/StatusBar.vue'
import { startRelay, relayActions } from './hooks/use-relay'
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
  if (!peer || peer === 'broadcast' || peer === 'self') {
    toast.error('先在左侧选择一台设备')
    return
  }
  if (peer === 'weixin') {
    toast.error('微信会话暂不支持拖拽发送文件')
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

/* ------------------------------ 设置（全屏页面：左标签菜单 + 右内容居中） ------------------------------ */
const showSettings = ref(false)
type SettingsTab = 'general' | 'remote' | 'proxy' | 'weixin'
const settingsTab = ref<SettingsTab>('general')
const settingsTabs: Array<{ key: SettingsTab; label: string; icon: typeof Settings2 }> = [
  { key: 'general', label: '常规', icon: Settings2 },
  { key: 'remote', label: '远程控制', icon: MonitorUp },
  { key: 'proxy', label: '代理', icon: Globe },
  { key: 'weixin', label: '微信', icon: MessageCircle },
]
const settingsForm = ref({
  device_name: '', port: 34256, save_dir: '', autostart: false, dnd_mode: false,
  stealth_mode: false,
  machine_id: '', remote_fixed_password: '', has_fixed_password: false,
  proxy_mode: 'system', proxy_url: '', no_proxy: '',
})

/* 代理三态：直连 / 跟随系统 / 自定义（企业网络常用） */
const proxyOptions = [
  { value: 'direct', label: '直连', desc: '不使用任何代理，直接连接网络' },
  { value: 'system', label: '跟随系统', desc: '读取操作系统代理设置与环境变量（默认）' },
  { value: 'custom', label: '自定义', desc: '手动指定代理地址与 NO_PROXY 例外列表' },
]

async function openSettings() {
  const s = await invoke<any>('get_settings').catch(() => null)
  if (s) {
    settingsForm.value = {
      device_name: s.device_name, port: s.port, save_dir: s.save_dir,
      autostart: s.autostart, dnd_mode: s.dnd_mode, stealth_mode: !!s.stealth_mode,
      machine_id: s.machine_id || '',
      remote_fixed_password: '', has_fixed_password: !!s.remote_fixed_password_hash,
      proxy_mode: ['direct', 'system', 'custom'].includes(s.proxy_mode) ? s.proxy_mode : 'system',
      proxy_url: s.proxy_url || '',
      no_proxy: s.no_proxy || '',
    }
    mutations.setRemoteMachineId(s.machine_id || null)
  }
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
      deviceName: s.device_name, port: s.port, saveDir: s.save_dir, autostart: s.autostart,
      dndMode: s.dnd_mode, stealthMode: s.stealth_mode,
      // 未输入新密码则不动现有固定密码
      remoteFixedPassword: s.remote_fixed_password === '' ? null : s.remote_fixed_password,
      proxyMode: s.proxy_mode, proxyUrl: s.proxy_url.trim(), noProxy: s.no_proxy.trim(),
    })
    store.meName = s.device_name
    store.dnd = s.dnd_mode
    store.stealth = s.stealth_mode
    if (s.remote_fixed_password !== '') s.has_fixed_password = true
    toast.success('设置已保存（端口、目录、代理重启后生效）')
    showSettings.value = false
  } catch (e) {
    toast.error(`保存失败：${String(e)}`)
  }
}
/** 清除固定密码（无人值守访问关闭） */
async function clearFixedPassword() {
  try {
    await invoke('save_settings', {
      deviceName: settingsForm.value.device_name, port: settingsForm.value.port,
      saveDir: settingsForm.value.save_dir, autostart: settingsForm.value.autostart,
      dndMode: settingsForm.value.dnd_mode, stealthMode: settingsForm.value.stealth_mode,
      remoteFixedPassword: '',
      proxyMode: settingsForm.value.proxy_mode, proxyUrl: settingsForm.value.proxy_url.trim(),
      noProxy: settingsForm.value.no_proxy.trim(),
    })
    settingsForm.value.has_fixed_password = false
    settingsForm.value.remote_fixed_password = ''
    toast.success('固定密码已清除')
  } catch (e) {
    toast.error(`清除失败：${String(e)}`)
  }
}

/* --------------------------- 微信通道：见 WeixinPanel 组件 --------------------------- */

/* --------------------------- 远程控制弹窗 --------------------------- */
const controlForm = ref({ password: '', connecting: false })
async function submitControl() {
  const d = store.controlDialog
  if (!d || controlForm.value.connecting) return
  if (!controlForm.value.password.trim()) {
    toast.error('请输入对方机器的密码')
    return
  }
  controlForm.value.connecting = true
  relayActions.remoteConnect(d.machine_id, controlForm.value.password.trim())
  // 连接结果由 remote_view_ready / remote_connect_failed 事件收尾
  setTimeout(() => { controlForm.value.connecting = false }, 4000)
}
function closeControlDialog() {
  mutations.closeControlDialog()
  controlForm.value = { password: '', connecting: false }
}
const currentAssist = computed(() => store.assistRequests[0] ?? null)

/* --------------------------- 手机观看（只看不控） --------------------------- */
const showPhoneView = ref(false)
const phoneViewQr = ref('')
const phoneViewUrl = ref('')
async function openPhoneView() {
  const v = store.remoteViewing
  if (!v || !v.ip) return
  phoneViewUrl.value = `http://${v.ip}:${v.port}/view?t=${v.view_token}`
  const QR = await import('qrcode')
  phoneViewQr.value = await QR.toDataURL(phoneViewUrl.value, { width: 220, margin: 1 })
  showPhoneView.value = true
}

/* ------------------------------ 生命周期 ------------------------------ */
onMounted(async () => {
  restoreTransferPanelPref()
  restoreChatStylePref()
  // 拖拽落点监听不依赖 startRelay 完成，先挂上（startRelay 里任何一步失败都不能影响它）
  window.addEventListener('yunshu-drop', onDrop)
  // 无边框窗口：标题栏双击/三击守卫 + 窗口状态跟随（最大化/全屏图标切换）
  installTauriDblclickGuard()
  initWindowControls()
  await startRelay()
  // 主界面直接展示本机机器 ID（远程控制/对方添加本机都要用）；临时密码走事件流
  invoke<any>('get_settings')
    .then((s) => {
      if (s) {
        mutations.setRemoteMachineId(s.machine_id || null)
        store.dnd = !!s.dnd_mode
        store.stealth = !!s.stealth_mode
      }
    })
    .catch(() => {})
})
</script>

<template>
  <div class="airpigeon-bg flex h-dvh flex-col overflow-hidden">
    <AppHeader
      @open-settings="openSettings"
      @toggle-sidebar="sidebarOpen = true"
    />

    <!-- 被控醒目横幅（安全要求：被控状态必须持续可见 + 一键断开） -->
    <div
      v-if="store.remoteControlled"
      class="flex h-10 shrink-0 items-center justify-between gap-3 bg-red-600 px-4 text-white"
      role="alert"
    >
      <div class="flex min-w-0 items-center gap-2 text-sm font-medium">
        <ShieldAlert class="size-4 shrink-0 animate-pulse" />
        <span class="truncate">「{{ store.remoteControlled.name }}」正在远程控制这台电脑</span>
      </div>
      <button
        class="shrink-0 rounded-lg bg-white/15 px-3 py-1 text-xs font-semibold hover:bg-white/25"
        @click="relayActions.remoteDisconnect()"
      >
        断开控制
      </button>
    </div>

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

      <!-- 中栏：默认「本机识别」首页（机器 ID/验证码 + 远程协助）；
           选中手机/主机后切为会话布局；选中「微信」切为微信对话界面 -->
      <div class="flex min-h-0 min-w-0 flex-1 flex-col">
        <LocalIdentityCard v-if="store.selectedPeer === 'self'" />
        <WeixinPanel v-else-if="store.selectedPeer === 'weixin'" class="bg-card/40" />
        <ChatPanel v-else :peer="store.selectedPeer" />
      </div>

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

    <!-- 手机观看入口（正在观看对方屏幕时显示） -->
    <button
      v-if="store.remoteViewing && !showPhoneView"
      class="fixed bottom-10 left-4 z-40 flex items-center gap-1.5 rounded-full bg-slate-800/90 px-3.5 py-2 text-xs font-medium text-white shadow-lg backdrop-blur hover:bg-slate-700"
      @click="openPhoneView"
    >
      <Smartphone class="size-3.5" />
      手机观看
    </button>

    <!-- 手机观看二维码弹窗 -->
    <div v-if="showPhoneView" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="showPhoneView = false">
      <div class="bg-card w-full max-w-sm rounded-xl border p-5 text-center shadow-xl">
        <p class="mb-1 text-sm font-semibold">手机观看「{{ store.remoteViewing?.name }}」</p>
        <p class="mb-3 text-xs text-muted-foreground">同一局域网的手机扫码即可只看不控</p>
        <img v-if="phoneViewQr" :src="phoneViewQr" alt="手机观看二维码" class="mx-auto rounded-lg border" />
        <p class="mt-3 break-all rounded-lg border bg-muted/30 px-3 py-2 text-[11px] text-muted-foreground">{{ phoneViewUrl }}</p>
        <div class="mt-4 flex justify-end">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="showPhoneView = false">关闭</button>
        </div>
      </div>
    </div>

    <!-- 远程协助请求（反向协助） -->
    <div v-if="currentAssist" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="relayActions.assistRespond(currentAssist.peer_id, false)">
      <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2.5 text-base font-semibold">
          <HandMetal class="size-5 text-amber-500" />
          远程协助请求
        </div>
        <p class="mb-1 text-sm">「{{ currentAssist.name }}」请求远程控制这台电脑</p>
        <p class="mb-4 text-xs text-muted-foreground">同意后将生成一次性密码并自动发送给对方，仅本次连接有效（5 分钟内）</p>
        <div class="flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="relayActions.assistRespond(currentAssist.peer_id, false)">拒绝</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700" @click="relayActions.assistRespond(currentAssist.peer_id, true)">
            允许控制
          </button>
        </div>
      </div>
    </div>

    <!-- 远程控制密码弹窗 -->
    <div v-if="store.controlDialog" class="fixed inset-0 z-50 flex items-center justify-center bg-foreground/45" @click.self="closeControlDialog">
      <div class="bg-card w-full max-w-md rounded-xl border p-5 shadow-xl">
        <div class="mb-1 flex items-center gap-2.5 text-base font-semibold">
          <MonitorUp class="size-5 text-emerald-600" />
          远程控制
        </div>
        <p class="mb-3 text-sm">正在连接「{{ store.controlDialog.name }}」</p>
        <label class="mb-1 block text-xs font-medium text-muted-foreground">对方机器 ID</label>
        <div class="mb-3 rounded-lg border bg-muted/30 px-3 py-2 font-mono text-sm tracking-widest">
          {{ store.controlDialog.machine_id }}
        </div>
        <label class="mb-1 block text-xs font-medium text-muted-foreground">对方密码（临时 / 固定 / 一次性）</label>
        <input
          v-model="controlForm.password"
          type="password"
          maxlength="32"
          placeholder="输入对方屏幕上显示的密码"
          class="mb-1 h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
          @keydown.enter="submitControl"
        />
        <p class="mb-4 text-[11px] text-muted-foreground">密码在加密通道内传输；连续输错 5 次将被锁定 10 分钟</p>
        <div class="flex justify-end gap-2">
          <button class="rounded-lg border px-4 py-2 text-sm hover:bg-accent" @click="closeControlDialog">取消</button>
          <button
            class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-60"
            :disabled="controlForm.connecting"
            @click="submitControl"
          >
            {{ controlForm.connecting ? '连接中…' : '连接' }}
          </button>
        </div>
      </div>
    </div>

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

    <!-- 设置：全屏页面（左标签菜单 + 右内容居中） -->
    <div v-if="showSettings" class="fixed inset-x-0 bottom-0 top-11 z-30 flex flex-col bg-background">
      <!-- 主体：左标签菜单 + 右面板 -->
      <div class="flex min-h-0 flex-1">
        <nav class="w-44 shrink-0 space-y-0.5 border-r p-2" aria-label="设置分类">
          <button
            v-for="t in settingsTabs"
            :key="t.key"
            type="button"
            role="tab"
            :aria-selected="settingsTab === t.key"
            :class="cn(
              'flex h-9 w-full items-center gap-2.5 rounded-lg px-2.5 text-left text-[13px] transition-colors',
              'focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none',
              settingsTab === t.key
                ? 'bg-emerald-500/12 font-medium text-emerald-700 dark:text-emerald-300'
                : 'text-muted-foreground hover:bg-accent hover:text-foreground'
            )"
            @click="settingsTab = t.key"
          >
            <component :is="t.icon" class="size-4 shrink-0" />
            {{ t.label }}
          </button>
        </nav>

        <div class="custom-scroll min-h-0 w-full flex-1 overflow-y-auto">
          <div class="mx-auto w-full max-w-xl px-6 py-6">
            <!-- 常规 -->
            <section v-if="settingsTab === 'general'" class="grid gap-3" aria-label="常规设置">
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
              <div class="mt-1 grid gap-2 border-t pt-3">
                <label class="flex items-center gap-2.5 text-sm">
                  <input v-model="settingsForm.autostart" type="checkbox" class="size-4 accent-emerald-600" />
                  开机自启
                </label>
                <label class="flex items-start gap-2.5 text-sm">
                  <input v-model="settingsForm.dnd_mode" type="checkbox" class="mt-0.5 size-4 accent-emerald-600" />
                  <span class="min-w-0">防打扰<span class="block text-xs text-muted-foreground">仅已信任设备可连接，广播消息静音；首次启动默认开启</span></span>
                </label>
                <label class="flex items-start gap-2.5 text-sm">
                  <input v-model="settingsForm.stealth_mode" type="checkbox" class="mt-0.5 size-4 accent-emerald-600" />
                  <span class="min-w-0">隐身模式<span class="block text-xs text-muted-foreground">不向局域网广播本机，其他设备的主机列表将移除本机；本机主动发送不受影响</span></span>
                </label>
              </div>
            </section>

            <!-- 远程控制 -->
            <section v-else-if="settingsTab === 'remote'" class="grid gap-3" aria-label="远程控制设置">
              <div class="grid grid-cols-2 gap-2.5">
                <label class="grid gap-1">
                  <span class="text-xs font-medium text-muted-foreground">本机机器 ID</span>
                  <div class="flex h-9 items-center rounded-lg border bg-muted/30 px-3 font-mono text-sm tracking-widest">
                    {{ settingsForm.machine_id || '——' }}
                  </div>
                </label>
                <label class="grid gap-1">
                  <span class="text-xs font-medium text-muted-foreground">临时密码（重启更换）</span>
                  <div class="flex h-9 items-center rounded-lg border bg-muted/30 px-3 font-mono text-sm tracking-widest">
                    {{ store.remoteTempPassword || '——' }}
                  </div>
                </label>
              </div>
              <label class="grid gap-1">
                <span class="text-xs font-medium text-muted-foreground">
                  固定密码{{ settingsForm.has_fixed_password ? '（已设置，输入可更换，留空保持不变）' : '（无人值守访问用，重启仍有效）' }}
                </span>
                <div class="flex gap-2">
                  <input
                    v-model="settingsForm.remote_fixed_password"
                    type="password"
                    maxlength="32"
                    :placeholder="settingsForm.has_fixed_password ? '••••••••（留空不变，输入即更换）' : '设置长期固定密码'"
                    autocomplete="new-password"
                    class="h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
                  />
                  <button
                    v-if="settingsForm.has_fixed_password"
                    class="h-9 shrink-0 rounded-lg border px-3 text-xs text-red-600 hover:bg-red-500/10 dark:text-red-400"
                    @click="clearFixedPassword"
                  >
                    清除
                  </button>
                </div>
              </label>
              <p class="text-[11px] leading-relaxed text-muted-foreground">
                其他 Yunshu 凭「机器 ID + 密码」即可控制本机（密码正确即授权）。macOS 需授予「屏幕录制」与「辅助功能」权限。
              </p>
            </section>

            <!-- 代理（企业网络）：直连 / 跟随系统 / 自定义 -->
            <section v-else-if="settingsTab === 'proxy'" class="grid gap-3" aria-label="代理设置">
              <p class="text-xs leading-relaxed text-muted-foreground">
                代理仅作用于微信通道等外网请求；局域网传输与远程控制不经过代理。修改保存后需重启 Yunshu 生效。
              </p>
              <div class="grid gap-2">
                <label
                  v-for="opt in proxyOptions"
                  :key="opt.value"
                  class="flex cursor-pointer items-start gap-2.5 rounded-lg border p-3 text-sm transition-colors"
                  :class="settingsForm.proxy_mode === opt.value ? 'border-emerald-500/50 bg-emerald-500/5' : 'hover:bg-accent'"
                >
                  <input
                    v-model="settingsForm.proxy_mode"
                    type="radio"
                    name="proxy-mode"
                    :value="opt.value"
                    class="mt-0.5 size-4 accent-emerald-600"
                  />
                  <span class="min-w-0">
                    <span class="font-medium">{{ opt.label }}</span>
                    <span class="block text-xs text-muted-foreground">{{ opt.desc }}</span>
                  </span>
                </label>
              </div>
              <template v-if="settingsForm.proxy_mode === 'custom'">
                <label class="grid gap-1">
                  <span class="text-xs font-medium text-muted-foreground">代理地址</span>
                  <input
                    v-model="settingsForm.proxy_url"
                    placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
                    class="h-9 rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
                  />
                </label>
                <label class="grid gap-1">
                  <span class="text-xs font-medium text-muted-foreground">NO_PROXY（不走代理的例外，逗号分隔）</span>
                  <input
                    v-model="settingsForm.no_proxy"
                    placeholder="localhost,127.0.0.1,.corp.example.com"
                    class="h-9 rounded-lg border bg-background px-3 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
                  />
                </label>
              </template>
            </section>

            <!-- 微信：复用主界面的微信会话面板（定高，内部自滚动） -->
            <section v-else class="h-[calc(100dvh-11rem)] min-h-0" aria-label="微信设置">
              <WeixinPanel />
            </section>
          </div>
        </div>
      </div>

      <!-- 底部：提示 + 操作 -->
      <div class="flex h-12 shrink-0 items-center justify-between gap-3 border-t px-5">
        <p class="min-w-0 text-xs text-muted-foreground">端口、目录、代理修改后需重启 Yunshu 生效</p>
        <div class="flex shrink-0 gap-2">
          <button class="rounded-lg border px-4 py-1.5 text-sm hover:bg-accent" @click="showSettings = false">取消</button>
          <button class="rounded-lg bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700" @click="saveSettings">保存</button>
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
          t.kind === 'error' ? 'bg-red-600/95 text-white'
            : t.kind === 'warning' ? 'bg-amber-500/95 text-white'
            : t.kind === 'success' ? 'bg-emerald-600/95 text-white'
            : 'bg-slate-800/95 text-white'
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
