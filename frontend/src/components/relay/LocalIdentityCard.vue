<script setup lang="ts">
/**
 * LocalIdentityCard：中栏「本机识别」首页视图（RustDesk 风格，占满整个中栏）
 * 大号展示机器 ID + 临时验证码（复制/眼睛开关），下方「远程协助他人」直连表单。
 * 选中手机/主机/广播时中栏切到 ChatPanel；点头部（或侧栏）自己的头像回到本视图。
 */
import { computed, ref } from 'vue'
import { Copy, Eye, EyeOff, MonitorUp } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import { store, myPresence } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'

/* 密码默认打码（防肩窥/录屏），点眼睛显示 */
const showPw = ref(false)
const idText = computed(() => store.remoteMachineId || '')
const idDisplay = computed(() => (idText.value ? idText.value.replace(/^(.{4})/, '$1 ') : '——'))
const pwDisplay = computed(() => (showPw.value ? store.remoteTempPassword || '——' : '••••••••'))

async function copyText(text: string, label: string) {
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
    toast.success(`${label}已复制`)
  } catch {
    // WebView 剪贴板权限兜底
    const ta = document.createElement('textarea')
    ta.value = text
    ta.style.position = 'fixed'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    ta.remove()
    toast.success(`${label}已复制`)
  }
}

/* 远程协助他人：凭机器 ID + 验证码直连（对方需在线：已互相发现或已在列表） */
const form = ref({ machineId: '', password: '' })
function connectPeer() {
  const id = form.value.machineId.trim().toLowerCase()
  const pw = form.value.password.trim()
  if (!/^[0-9a-f]{8}$/.test(id)) {
    toast.error('请输入 8 位机器 ID（对方主界面或设置里可查看）')
    return
  }
  if (!pw) {
    toast.error('请输入对方的验证码')
    return
  }
  relayActions.remoteConnect(id, pw)
  form.value.password = ''
}
</script>

<template>
  <section
    aria-label="本机识别"
    class="bg-card/40 relative flex min-h-0 min-w-0 flex-1 flex-col"
  >
    <!-- 头部（与会话头同规格）：自己的头像 + 设备名/IP -->
    <div class="flex h-11 shrink-0 items-center gap-2.5 border-b bg-card/70 px-3 backdrop-blur-sm">
      <RelayAvatar :name="store.meName" size="sm" :presence="myPresence()" />
      <p class="min-w-0 flex-1 truncate text-sm font-semibold">
        {{ store.meName }}
        <span v-if="store.meIp" class="ml-2 font-mono text-xs font-normal text-muted-foreground">{{ store.meIp }}</span>
        <span class="ml-2 text-xs font-normal text-muted-foreground">本机识别</span>
      </p>
    </div>

    <div class="custom-scroll flex-1 overflow-y-auto px-4 py-6">
      <div class="mx-auto flex w-full max-w-xl flex-col gap-4">
        <!-- 识别码 + 验证码 -->
        <div class="rounded-2xl border bg-card p-6 shadow-sm">
          <!-- 本机识别码 -->
          <p class="text-xs text-muted-foreground">本机识别码</p>
          <div class="mt-1.5 flex items-center gap-3">
            <span class="min-w-0 flex-1 truncate font-mono text-4xl font-bold tracking-[6px]">{{ idDisplay }}</span>
            <button
              type="button"
              class="flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="复制机器 ID"
              title="复制"
              @click="copyText(idText, '机器 ID')"
            >
              <Copy class="size-4.5" />
            </button>
          </div>

          <div class="bg-border my-5 h-px" aria-hidden="true"></div>

          <!-- 临时验证码 -->
          <p class="text-xs text-muted-foreground">
            临时验证码 <span class="text-[10px]">（重启更换）</span>
          </p>
          <div class="mt-1.5 flex items-center gap-3">
            <span class="min-w-0 flex-1 truncate font-mono text-4xl font-bold tracking-[6px]">{{ pwDisplay }}</span>
            <button
              type="button"
              class="flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground"
              :aria-label="showPw ? '隐藏验证码' : '显示验证码'"
              :title="showPw ? '隐藏' : '显示'"
              @click="showPw = !showPw"
            >
              <EyeOff v-if="showPw" class="size-4.5" />
              <Eye v-else class="size-4.5" />
            </button>
            <button
              type="button"
              class="flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="复制验证码"
              title="复制"
              @click="copyText(store.remoteTempPassword || '', '验证码')"
            >
              <Copy class="size-4.5" />
            </button>
          </div>

          <p class="mt-4 text-[11px] leading-relaxed text-muted-foreground">
            其他 Yunshu（电脑或手机）凭这两项即可远程控制本机；验证码正确即授权，连续输错将锁定
          </p>
        </div>

        <!-- 远程协助他人 -->
        <div class="rounded-2xl border bg-card p-5 shadow-sm">
          <p class="flex items-center gap-1.5 text-sm font-semibold">
            <MonitorUp class="size-4 text-emerald-600" />
            远程协助他人
          </p>
          <div class="mt-3 flex gap-2">
            <input
              v-model="form.machineId"
              placeholder="请输入伙伴机器 ID（8 位）"
              maxlength="8"
              class="h-9 min-w-0 flex-1 rounded-lg border bg-background px-3 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              @keydown.enter="connectPeer"
            />
            <span class="flex items-center text-xs text-muted-foreground">—</span>
            <input
              v-model="form.password"
              type="password"
              placeholder="验证码"
              maxlength="32"
              class="h-9 w-36 rounded-lg border bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              @keydown.enter="connectPeer"
            />
            <button
              type="button"
              class="h-9 shrink-0 rounded-lg bg-emerald-600 px-5 text-sm font-medium text-white hover:bg-emerald-700"
              @click="connectPeer"
            >
              连接
            </button>
          </div>
          <p class="mt-2 text-[11px] text-muted-foreground">
            对方需在线（已互相发现或已在设备列表中）；也可在左侧设备行右键发起协助
          </p>
        </div>
      </div>
    </div>
  </section>
</template>
