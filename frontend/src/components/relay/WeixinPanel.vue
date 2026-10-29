<script setup lang="ts">
/**
 * WeixinPanel：微信通道面板 — 扫码登录（二维码/配对数字）+ 已登录后的微信对话界面
 * 主界面侧栏「微信」会话与设置弹窗「微信」页共用；登录任务在后端，
 * 本组件重挂载时自动恢复登录视图与轮询
 */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import {
  FileText, Film, FolderOpen, Image as ImageIcon, LoaderCircle, LogOut,
  MessageCircle, Music, Send,
} from '@lucide/vue'
import { store, mutations } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'
import { formatBytes } from '../../lib/relay-format'
import { cn } from '../../lib/utils'

/* ------------------------------ 登录流程 ------------------------------ */
const wxStatus = ref<any>(null)
const wxLogin = ref<any>(null)
const wxQrData = ref('')
const wxVerifyCode = ref('')
let wxPollTimer: ReturnType<typeof setInterval> | null = null

async function renderWxQr(url: string) {
  const QR = await import('qrcode')
  wxQrData.value = await QR.toDataURL(url, { width: 220, margin: 1 })
}

/** 拉微信状态；后端正处在登录流程中时恢复二维码视图与轮询 */
async function refreshWx() {
  wxStatus.value = await invoke<any>('weixin_status').catch(() => null)
  mutations.setWeixinLoggedIn(!!wxStatus.value?.logged_in)
  if (wxStatus.value?.logged_in) {
    const list = await invoke<any[]>('weixin_inbox', { limit: 50 }).catch(() => [])
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

onMounted(refreshWx)
onUnmounted(wxStopWxPoll)

/* ------------------------------ 对话时间线 ------------------------------ */

type WxEntry = {
  at_ms: number
  dir: 'in' | 'out'
  kind: string
  text: string
  from?: string
  file_name?: string
  size?: number
  saved_path?: string
  error?: string
}

const timeline = computed<WxEntry[]>(() => [
  ...store.weixinInbound.map((m) => ({ ...m, dir: 'in' as const })),
  ...store.weixinOutbound.map((m) => ({ at_ms: m.at_ms, dir: 'out' as const, kind: 'text', text: m.text })),
].sort((a, b) => a.at_ms - b.at_ms))

function fmtTime(ms: number) {
  const d = new Date(ms)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

/** ilink 用户 ID 太长，界面只显示首尾片段 */
function shortId(id?: string) {
  if (!id) return '微信用户'
  return id.length > 14 ? `${id.slice(0, 7)}…${id.slice(-4)}` : id
}

function kindIcon(kind: string) {
  if (kind === 'image') return ImageIcon
  if (kind === 'video') return Film
  if (kind === 'voice') return Music
  return FileText
}

const pollLabel = computed(() => {
  const s = wxStatus.value
  if (!s) return ''
  if (s.last_error) return `最近错误：${s.last_error}`
  if (s.polling) return '在线 · 消息实时同步'
  if (s.paused_remaining_ms > 0) return `冷却中（约 ${Math.ceil(s.paused_remaining_ms / 60000)} 分钟，重新扫码可解除）`
  return '轮询已停止，重新扫码可恢复'
})

/* ------------------------------ 发送 ------------------------------ */
const draft = ref('')
const sending = ref(false)

async function sendDraft() {
  const text = draft.value.trim()
  if (!text || sending.value) return
  sending.value = true
  try {
    await invoke('weixin_send_text', { text })
    mutations.addWeixinOutbound(text)
    draft.value = ''
  } catch (e) {
    toast.error(`发送失败：${String(e)}`)
  } finally {
    sending.value = false
  }
}
</script>

<template>
  <section class="flex h-full min-h-0 min-w-0 flex-1 flex-col" aria-label="微信会话">
    <!-- 未登录：引导扫码；登录中：二维码 / 配对数字 -->
    <div v-if="!store.weixinLoggedIn" class="custom-scroll flex flex-1 items-center overflow-y-auto p-4">
      <div class="mx-auto w-full max-w-sm text-center">
        <template v-if="wxLogin">
          <img
            v-if="wxQrData"
            :src="wxQrData"
            alt="微信登录二维码"
            class="mx-auto rounded-xl border bg-white p-2 shadow-sm"
          />
          <div
            v-else
            class="mx-auto flex h-56 w-56 items-center justify-center rounded-xl border bg-muted text-xs text-muted-foreground"
          >
            <LoaderCircle class="size-5 animate-spin" />
          </div>
          <p class="mt-3 text-sm text-muted-foreground">{{ wxLogin.message || '请用手机微信扫描二维码' }}</p>
          <div v-if="wxLogin.status === 'need_verifycode'" class="mt-3 flex gap-2">
            <input
              v-model="wxVerifyCode"
              inputmode="numeric"
              maxlength="8"
              placeholder="输入手机微信上显示的配对数字"
              class="h-9 w-full rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              @keydown.enter="wxSubmitCode"
            />
            <button
              class="h-9 shrink-0 rounded-lg bg-emerald-600 px-3 text-sm font-medium text-white hover:bg-emerald-700"
              @click="wxSubmitCode"
            >
              确认
            </button>
          </div>
          <button class="mt-3 h-8 rounded-lg border px-3 text-xs hover:bg-accent" @click="wxCancelLogin">
            取消登录
          </button>
        </template>
        <template v-else>
          <div class="bg-card mx-auto flex flex-col items-center gap-3 rounded-2xl border p-6 shadow-sm">
            <span class="flex size-12 items-center justify-center rounded-2xl bg-gradient-to-br from-emerald-400 via-emerald-500 to-teal-600 text-white shadow-lg shadow-emerald-500/25">
              <MessageCircle class="size-6" />
            </span>
            <div>
              <p class="text-sm font-semibold">连接微信</p>
              <p class="mt-1 text-xs leading-relaxed text-muted-foreground">
                扫码绑定个人微信机器人：微信里收到的消息与文件实时转到 Yunshu（文件自动解密落盘），
                电脑上的内容也能直接回复到你的微信
              </p>
            </div>
            <button
              class="h-9 w-full rounded-lg border border-emerald-600/40 bg-emerald-600/10 text-sm font-medium text-emerald-700 hover:bg-emerald-600/20 dark:text-emerald-400"
              @click="wxLoginStart"
            >
              扫码登录微信
            </button>
          </div>
        </template>
      </div>
    </div>

    <!-- 已登录：对话界面 -->
    <template v-else>
      <!-- 会话头：状态 + 退出 -->
      <div class="flex h-11 shrink-0 items-center gap-2.5 border-b bg-card/70 px-3 backdrop-blur-sm">
        <span class="flex size-7 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br from-emerald-400 to-teal-600 text-white" aria-hidden="true">
          <MessageCircle class="size-3.5" />
        </span>
        <p class="min-w-0 flex-1 truncate text-sm font-semibold">
          微信
          <span class="ml-2 text-xs font-normal" :class="wxStatus?.last_error ? 'text-red-500' : 'text-muted-foreground'">
            {{ pollLabel }}
          </span>
        </p>
        <button
          class="text-muted-foreground flex size-8 shrink-0 items-center justify-center rounded-lg transition-colors hover:bg-accent hover:text-red-600"
          title="退出微信登录"
          aria-label="退出微信登录"
          @click="wxLogout"
        >
          <LogOut class="size-4" />
        </button>
      </div>

      <!-- 消息流 -->
      <div class="custom-scroll min-h-0 flex-1 overflow-y-auto p-4">
        <div
          v-if="timeline.length === 0"
          class="flex h-full flex-col items-center justify-center gap-1.5 text-center"
        >
          <MessageCircle class="size-8 text-muted-foreground/40" stroke-width="1.5" />
          <p class="text-xs text-muted-foreground">微信消息会实时出现在这里</p>
          <p class="max-w-[240px] text-[10px] leading-relaxed text-muted-foreground/70">
            在微信里给机器人发条消息即可双向对话；收到的文件自动存到「保存目录 / 微信 /」
          </p>
        </div>
        <div v-else class="space-y-3">
          <div
            v-for="(m, i) in timeline"
            :key="`${m.dir}-${m.at_ms}-${i}`"
            class="flex"
            :class="m.dir === 'out' ? 'justify-end' : 'justify-start'"
          >
            <div class="max-w-[78%]">
              <p
                v-if="m.dir === 'in'"
                class="mb-0.5 text-[10px] text-muted-foreground"
                :title="m.from"
              >
                {{ shortId(m.from) }} · {{ fmtTime(m.at_ms) }}
              </p>
              <!-- 媒体：文件卡片（图片/视频/文件），文本走气泡 -->
              <div
                v-if="m.kind !== 'text'"
                class="bg-card inline-flex items-center gap-2.5 rounded-xl border px-3 py-2 shadow-sm"
              >
                <span class="bg-muted text-muted-foreground flex size-8 shrink-0 items-center justify-center rounded-lg">
                  <component :is="kindIcon(m.kind)" class="size-4" />
                </span>
                <span class="min-w-0">
                  <span class="block truncate text-[13px]" :title="m.file_name || m.saved_path">
                    {{ m.file_name || m.saved_path?.split('/').pop() || m.text }}
                  </span>
                  <span class="text-muted-foreground block text-[10px]">
                    <template v-if="m.error">下载失败：{{ m.error }}</template>
                    <template v-else-if="m.size">{{ formatBytes(m.size) }}</template>
                    <template v-else>{{ m.kind }}</template>
                  </span>
                </span>
                <button
                  v-if="m.saved_path"
                  class="text-muted-foreground hover:text-foreground flex size-7 shrink-0 items-center justify-center rounded-lg hover:bg-accent"
                  title="打开所在文件夹"
                  aria-label="打开所在文件夹"
                  @click="relayActions.openFolder(m.saved_path!)"
                >
                  <FolderOpen class="size-3.5" />
                </button>
              </div>
              <p
                v-else
                :class="cn(
                  'inline-block rounded-xl px-3 py-1.5 text-[13px] leading-relaxed whitespace-pre-wrap break-words shadow-sm',
                  m.dir === 'out'
                    ? 'bg-gradient-to-br from-emerald-500 to-emerald-600 text-white'
                    : 'bg-card border'
                )"
              >{{ m.text }}</p>
              <p v-if="m.dir === 'out'" class="text-muted-foreground mt-0.5 text-right text-[10px]">
                {{ fmtTime(m.at_ms) }}
              </p>
            </div>
          </div>
        </div>
      </div>

      <!-- 输入区 -->
      <div class="shrink-0 border-t bg-card/60 p-2 backdrop-blur-sm sm:p-2.5">
        <div class="flex items-end gap-2">
          <textarea
            v-model="draft"
            rows="1"
            placeholder="发到微信，Enter 发送…"
            aria-label="微信消息输入框"
            title="Enter 发送 · Shift+Enter 换行"
            :class="cn(
              'custom-scroll field-sizing-content max-h-28 min-h-10 w-full resize-none rounded-lg border bg-background px-3 py-2 text-[13px] shadow-xs',
              'transition-[color,box-shadow] outline-none placeholder:text-muted-foreground',
              'focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50'
            )"
            @keydown.enter.exact.prevent="sendDraft"
          ></textarea>
          <button
            aria-label="发送到微信"
            :disabled="!draft.trim() || sending"
            :class="cn(
              'flex size-10 shrink-0 items-center justify-center rounded-lg transition-transform md:size-9',
              draft.trim() && !sending
                ? 'bg-gradient-to-r from-emerald-500 to-emerald-600 text-white shadow-sm shadow-emerald-500/30 hover:scale-105 active:scale-95'
                : 'bg-muted text-muted-foreground cursor-not-allowed'
            )"
            @click="sendDraft"
          >
            <LoaderCircle v-if="sending" class="size-4 animate-spin" />
            <Send v-else class="size-4" />
          </button>
        </div>
      </div>
    </template>
  </section>
</template>
