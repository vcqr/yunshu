<script setup lang="ts">
/**
 * 云书 — 主页面（复刻 demo/ui page.tsx）
 * 桌面软件式布局：紧凑标题栏 / 设备侧栏(md+) / 聊天主区 / 传输面板(lg+ 第三栏) / 底部状态栏
 * 云书已有功能：手机扫码配对（会话内视图）、TOFU 信任确认、设置、打开所在文件夹
 */
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { Bird, HandMetal, MonitorUp, ShieldAlert, ShieldCheck, Smartphone } from '@lucide/vue'
import AppHeader from './components/relay/AppHeader.vue'
import DeviceSidebar from './components/relay/DeviceSidebar.vue'
import LocalIdentityCard from './components/relay/LocalIdentityCard.vue'
import ChatPanel from './components/relay/ChatPanel.vue'
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
const settingsForm = ref({
  device_name: '', port: 34256, save_dir: '', autostart: false, dnd_mode: false,
  stealth_mode: false,
  machine_id: '', remote_fixed_password: '', has_fixed_password: false,
})
async function openSettings() {
  const s = await invoke<any>('get_settings').catch(() => null)
  if (s) {
    settingsForm.value = {
      device_name: s.device_name, port: s.port, save_dir: s.save_dir,
      autostart: s.autostart, dnd_mode: s.dnd_mode, stealth_mode: !!s.stealth_mode,
      machine_id: s.machine_id || '',
      remote_fixed_password: '', has_fixed_password: !!s.remote_fixed_password_hash,
    }
    mutations.setRemoteMachineId(s.machine_id || null)
  }
  showSettings.value = true
  void refreshWx()
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
    })
    store.meName = s.device_name
    store.dnd = s.dnd_mode
    store.stealth = s.stealth_mode
    if (s.remote_fixed_password !== '') s.has_fixed_password = true
    toast.success('设置已保存（端口、目录重启后生效）')
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
    })
    settingsForm.value.has_fixed_password = false
    settingsForm.value.remote_fixed_password = ''
    toast.success('固定密码已清除')
  } catch (e) {
    toast.error(`清除失败：${String(e)}`)
  }
}

/* --------------------------- 微信通道（扫码登录个人微信机器人） --------------------------- */
const wxStatus = ref<any>(null)
const wxLogin = ref<any>(null)
const wxQrData = ref('')
const wxVerifyCode = ref('')
let wxPollTimer: ReturnType<typeof setInterval> | null = null

async function renderWxQr(url: string) {
  const QR = await import('qrcode')
  wxQrData.value = await QR.toDataURL(url, { width: 220, margin: 1 })
}

/** 拉微信状态；若后端正处在登录流程中（重开设置窗时），恢复二维码视图与轮询 */
async function refreshWx() {
  wxStatus.value = await invoke<any>('weixin_status').catch(() => null)
  if (wxStatus.value?.logged_in) {
    const list = await invoke<any[]>('weixin_inbox', { limit: 5 }).catch(() => [])
    const seen = new Set(store.weixinInbound.map((m) => `${m.at_ms}|${m.from}`))
    for (const m of list) {
      if (!seen.has(`${m.at_ms}|${m.from}`)) mutations.addWeixinInbound(m)
    }
  }
  const lv = await invoke<any>('weixin_login_status').catch(() => null)
  if (lv && ['waiting', 'scanned', 'need_verifycode'].includes(lv.status)) {
    wxLogin.value = lv
    if (lv.qr_url && !wxQrData.value) void renderWxQr(lv.qr_url)
    if (!wxPollTimer) wxPollTimer = setInterval(wxPollOnce, 1500)
  }
}

async function wxLoginStart() {
  const v = await invoke<any>('weixin_login_start').catch((e) => {
    toast.error(String(e))
    return null
  })
  if (!v) return
  wxLogin.value = v
  wxVerifyCode.value = ''
  wxQrData.value = ''
  if (v.qr_url) void renderWxQr(v.qr_url)
  if (!wxPollTimer) wxPollTimer = setInterval(wxPollOnce, 1500)
}

async function wxPollOnce() {
  const v = await invoke<any>('weixin_login_status').catch(() => null)
  if (!v) return
  wxLogin.value = v
  const st = v.status as string
  if (st === 'confirmed' || st === 'binded') {
    wxStopWxPoll()
    wxLogin.value = null
    wxQrData.value = ''
    await refreshWx()
    toast.success('微信已连接')
  } else if (st === 'failed') {
    wxStopWxPoll()
    wxQrData.value = ''
  }
}

function wxStopWxPoll() {
  if (wxPollTimer) {
    clearInterval(wxPollTimer)
    wxPollTimer = null
  }
}

/** need_verifycode：提交手机微信上显示的配对数字 */
async function wxSubmitCode() {
  const code = wxVerifyCode.value.trim()
  if (!code) return
  await invoke('weixin_login_submit_code', { code }).catch((e) => toast.error(String(e)))
}

function wxCancelLogin() {
  invoke('weixin_login_cancel')
  wxStopWxPoll()
  wxLogin.value = null
  wxQrData.value = ''
}

async function wxLogout() {
  await invoke('weixin_logout').catch((e) => toast.error(String(e)))
  wxStopWxPoll()
  wxLogin.value = null
  wxQrData.value = ''
  await refreshWx()
  toast.success('已退出微信登录')
}

/* 设置弹窗关闭即停轮询（登录任务在后端继续，重开可恢复视图） */
watch(showSettings, (v) => {
  if (!v) wxStopWxPoll()
})

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
           选中手机或主机后整个中栏切为会话布局 -->
      <div class="flex min-h-0 min-w-0 flex-1 flex-col">
        <LocalIdentityCard v-if="store.selectedPeer === 'self'" />
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
            防打扰（仅已信任设备可连接，广播消息静音；首次启动默认开启）
          </label>
          <label class="flex items-center gap-2.5 text-sm">
            <input v-model="settingsForm.stealth_mode" type="checkbox" class="size-4 accent-emerald-600" />
            隐身模式（不向局域网广播本机，其他设备的主机列表将移除本机；本机主动发送不受影响）
          </label>

          <div class="mt-2 border-t pt-3">
            <p class="mb-2 text-xs font-semibold text-foreground">远程控制</p>
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
            <label class="mt-2.5 grid gap-1">
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
            <p class="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
              其他云书凭「机器 ID + 密码」即可控制本机（密码正确即授权）。macOS 需授予「屏幕录制」与「辅助功能」权限。
            </p>
          </div>

          <div class="mt-2 border-t pt-3">
            <p class="mb-2 text-xs font-semibold text-foreground">微信通道</p>
            <!-- 登录中：二维码 / 配对数字 / 取消 -->
            <div v-if="wxLogin" class="grid justify-items-center gap-2">
              <img v-if="wxQrData" :src="wxQrData" alt="微信登录二维码" class="size-44 rounded-lg border bg-white p-1.5" />
              <p class="text-center text-xs text-muted-foreground">{{ wxLogin.message || '请用手机微信扫描二维码' }}</p>
              <div v-if="wxLogin.status === 'need_verifycode'" class="flex w-full gap-2">
                <input
                  v-model="wxVerifyCode"
                  inputmode="numeric"
                  maxlength="8"
                  placeholder="输入手机微信上显示的配对数字"
                  class="h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
                />
                <button class="h-9 shrink-0 rounded-lg bg-emerald-600 px-3 text-sm font-medium text-white hover:bg-emerald-700" @click="wxSubmitCode">确认</button>
              </div>
              <button class="h-8 rounded-lg border px-3 text-xs hover:bg-accent" @click="wxCancelLogin">取消登录</button>
            </div>
            <!-- 已登录：状态 + 最近收到 -->
            <div v-else-if="wxStatus?.logged_in" class="grid gap-2">
              <div class="flex items-start justify-between gap-2">
                <div class="min-w-0 text-xs leading-relaxed text-muted-foreground">
                  <p class="font-medium text-foreground">✓ 已连接微信</p>
                  <p>轮询：{{ wxStatus.polling ? '运行中' : wxStatus.paused_remaining_ms > 0 ? '冷却中（约 ' + Math.ceil(wxStatus.paused_remaining_ms / 60000) + ' 分钟，重新扫码可解除）' : '已停止' }}</p>
                  <p v-if="wxStatus.last_error" class="text-red-500">最近错误：{{ wxStatus.last_error }}</p>
                  <p>收到的媒体自动存到「保存目录/微信/」</p>
                </div>
                <button class="h-8 shrink-0 rounded-lg border px-3 text-xs text-red-600 hover:bg-red-500/10 dark:text-red-400" @click="wxLogout">退出登录</button>
              </div>
              <div v-if="store.weixinInbound.length" class="grid gap-1 rounded-lg bg-muted/40 p-2">
                <p class="text-[11px] font-medium text-muted-foreground">最近收到</p>
                <p
                  v-for="(m, i) in [...store.weixinInbound].slice(-4).reverse()"
                  :key="i"
                  class="truncate text-[11px] text-muted-foreground"
                  :title="m.saved_path || m.text"
                >
                  ← {{ m.text }}
                </p>
              </div>
              <p class="text-[11px] leading-relaxed text-muted-foreground">
                微信里收到的消息与文件会实时转发到云书；电脑上的内容可用 yunshu-cli（wx … 命令）或 AI agent 的 weixin_* 工具发到你的微信。
              </p>
            </div>
            <!-- 未登录 -->
            <template v-else>
              <button
                class="h-9 w-full rounded-lg border border-emerald-600/40 bg-emerald-600/10 text-sm font-medium text-emerald-700 hover:bg-emerald-600/20 dark:text-emerald-400"
                @click="wxLoginStart"
              >
                扫码登录微信
              </button>
              <p class="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
                用手机微信扫码绑定个人微信机器人：此后微信里收到的消息与文件实时转到云书（文件自动解密落盘）；电脑上的内容也能通过 CLI/AI agent 发回你的微信。
              </p>
            </template>
          </div>
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
