<script setup lang="ts">
/**
 * LocalIdentityCard：中栏顶部的本机识别卡（RustDesk 风格）
 * 大号展示机器 ID + 临时验证码（复制/眼睛开关），下方「远程协助他人」直连表单。
 * 可折叠（记忆在 localStorage），默认展开。
 */
import { computed, ref } from 'vue'
import { ChevronDown, ChevronUp, Copy, Eye, EyeOff, MonitorUp } from '@lucide/vue'
import { store } from '../../stores/relay-store'
import { relayActions } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'

const COLLAPSE_KEY = 'ys-idcard-collapsed'
const collapsed = ref(localStorage.getItem(COLLAPSE_KEY) === '1')
function toggleCollapse() {
  collapsed.value = !collapsed.value
  try { localStorage.setItem(COLLAPSE_KEY, collapsed.value ? '1' : '0') } catch { /* 隐私模式等 */ }
}

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
  <div class="bg-card/60 relative shrink-0 border-b px-4 py-3 backdrop-blur">
    <!-- 折叠/展开按钮 -->
    <button
      type="button"
      class="absolute right-2 top-2 flex size-6 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
      :aria-label="collapsed ? '展开本机识别卡' : '折叠本机识别卡'"
      :title="collapsed ? '展开' : '折叠'"
      @click="toggleCollapse"
    >
      <ChevronDown v-if="!collapsed" class="size-4" />
      <ChevronUp v-else class="size-4" />
    </button>

    <template v-if="!collapsed">
      <div class="grid grid-cols-1 gap-3 pr-8 sm:grid-cols-2">
        <!-- 本机识别码 -->
        <div>
          <p class="text-xs text-muted-foreground">本机识别码</p>
          <div class="mt-0.5 flex items-center gap-2">
            <span class="truncate font-mono text-2xl font-bold tracking-widest">{{ idDisplay }}</span>
            <button
              type="button"
              class="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="复制机器 ID"
              title="复制"
              @click="copyText(idText, '机器 ID')"
            >
              <Copy class="size-4" />
            </button>
          </div>
        </div>
        <!-- 临时验证码 -->
        <div>
          <p class="text-xs text-muted-foreground">
            临时验证码 <span class="text-[10px]">（重启更换）</span>
          </p>
          <div class="mt-0.5 flex items-center gap-2">
            <span class="truncate font-mono text-2xl font-bold tracking-widest">{{ pwDisplay }}</span>
            <button
              type="button"
              class="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              :aria-label="showPw ? '隐藏验证码' : '显示验证码'"
              :title="showPw ? '隐藏' : '显示'"
              @click="showPw = !showPw"
            >
              <EyeOff v-if="showPw" class="size-4" />
              <Eye v-else class="size-4" />
            </button>
            <button
              type="button"
              class="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="复制验证码"
              title="复制"
              @click="copyText(store.remoteTempPassword || '', '验证码')"
            >
              <Copy class="size-4" />
            </button>
          </div>
        </div>
      </div>

      <!-- 远程协助他人 -->
      <div class="mt-3 border-t pt-2.5">
        <p class="flex items-center gap-1.5 text-sm font-semibold">
          <MonitorUp class="size-4 text-emerald-600" />
          远程协助他人
        </p>
        <div class="mt-2 flex gap-2">
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
        <p class="mt-1.5 text-[11px] text-muted-foreground">
          对方需在线（已互相发现或已在设备列表中）；密码正确即授权，连续输错将锁定
        </p>
      </div>
    </template>

    <!-- 折叠态：单行摘要 -->
    <div v-else class="flex items-center justify-between gap-2 pr-8">
      <p class="truncate font-mono text-xs text-muted-foreground">
        本机 ID <span class="text-foreground">{{ idDisplay }}</span>
        · 验证码 <span class="text-foreground">{{ pwDisplay }}</span>
      </p>
    </div>
  </div>
</template>
