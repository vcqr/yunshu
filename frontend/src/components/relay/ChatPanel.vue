<script setup lang="ts">
/**
 * ChatPanel：会话主体 — 单行紧凑标题 + 消息流 + 输入区 + 拖拽遮罩
 * 手机未配对时显示扫码配对视图（云书特有）
 */
import { computed, ref, watch } from 'vue'
import QRCode from 'qrcode'
import { ListOrdered, MessageSquare, Radio, Smartphone, UploadCloud } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import MessageList from './MessageList.vue'
import Composer from './Composer.vue'
import { store, filterEntriesForPeer, setChatStylePersist } from '../../stores/relay-store'
import { relayActions, dragState } from '../../hooks/use-relay'
import { toast } from '../../hooks/use-toast'

const props = defineProps<{ peer: string }>()

const isPhone = computed(() => props.peer === 'phone')
const isBroadcast = computed(() => props.peer === 'broadcast')
const peerDevice = computed(() => store.devices.find((d) => d.id === props.peer) ?? null)
const peerName = computed(() =>
  isBroadcast.value ? '所有人' : isPhone.value ? (store.phoneConnected ? store.phoneName : '手机') : peerDevice.value?.name ?? '对方设备'
)
const peerOnline = computed(() =>
  isBroadcast.value ? true : isPhone.value ? store.phoneConnected : Boolean(peerDevice.value)
)
const peerIp = computed(() => (isPhone.value ? '' : peerDevice.value?.ip ?? ''))

const entries = computed(() => filterEntriesForPeer(props.peer))

/* 手机未配对：显示扫码配对视图 */
const qrData = ref<string | null>(null)
watch(
  () => store.qrUrl,
  (url) => {
    if (url) QRCode.toDataURL(url, { width: 460, margin: 1 }).then((d) => { qrData.value = d })
  },
  { immediate: true }
)

/* 复制备用直达地址 */
async function copyAlt(u: string) {
  try {
    await navigator.clipboard.writeText(u)
    toast.info(`已复制：${u}`)
  } catch {
    toast.info(u)
  }
}

/* 拖拽遮罩状态（由 use-relay 的全局拖拽事件驱动） */
const dragActive = computed(() => dragState.active && store.selectedPeer === props.peer)

function switchChatStyle() {
  setChatStylePersist(store.chatStyle === 'bubble' ? 'list' : 'bubble')
}
</script>

<template>
  <section
    :aria-label="`与 ${peerName} 的会话`"
    class="bg-card/40 relative flex min-h-0 min-w-0 flex-1 flex-col"
  >
    <!-- 手机未配对：扫码配对视图 -->
    <template v-if="isPhone && !store.phoneConnected">
      <div class="flex h-11 shrink-0 items-center gap-2.5 border-b bg-card/70 px-3 backdrop-blur-sm">
        <div class="bg-muted text-muted-foreground flex size-7 shrink-0 items-center justify-center rounded-lg">
          <Smartphone class="size-3.5" />
        </div>
        <p class="min-w-0 flex-1 truncate text-sm font-semibold">
          手机
          <span class="ml-2 text-xs font-normal text-muted-foreground">扫码配对后即可互传文件与文本</span>
        </p>
      </div>
      <div class="custom-scroll flex-1 overflow-y-auto p-4">
        <div
          v-if="store.pairPending"
          class="mx-auto max-w-sm rounded-2xl border border-emerald-500/40 bg-emerald-500/5 p-5 text-center shadow-sm"
        >
          <p class="text-sm font-medium">手机「{{ store.pairPending.device }}」请求配对</p>
          <p class="mt-1 text-xs text-muted-foreground">电脑无需操作：把配对码输入到手机即可</p>
          <div class="my-3 font-mono text-4xl font-bold tracking-[8px] text-emerald-700 dark:text-emerald-300">
            {{ store.pairPending.code }}
          </div>
        </div>
        <div class="mx-auto mt-4 flex max-w-sm flex-col items-center gap-2 rounded-2xl border bg-card p-4 shadow-sm">
          <img v-if="qrData" :src="qrData" alt="扫码连接" width="240" />
          <div v-else class="flex h-[240px] w-[240px] items-center justify-center rounded-lg bg-muted text-xs text-muted-foreground">
            二维码生成中…
          </div>
          <p class="text-xs text-muted-foreground">手机扫码 · 浏览器打开 · 免安装</p>
          <!-- 扫码地址不对时的备用直达地址（全部网卡，随配对码轮换刷新） -->
          <div v-if="store.qrAlts.length" class="mt-2 w-[240px] space-y-1">
            <p class="text-[10px] font-medium text-muted-foreground">扫不上？手动访问（点击复制）：</p>
            <button
              v-for="u in store.qrAlts"
              :key="u"
              class="block w-full truncate rounded bg-muted/40 px-2 py-1 text-left text-[10px] text-muted-foreground transition-colors hover:text-foreground"
              :title="u"
              @click="copyAlt(u)"
            >
              {{ u }}
            </button>
          </div>
        </div>
      </div>
    </template>

    <!-- 正常会话 -->
    <template v-else>
      <!-- 单行紧凑会话头 -->
      <div class="flex h-11 shrink-0 items-center gap-2.5 border-b bg-card/70 px-3 backdrop-blur-sm">
        <span
          v-if="isBroadcast"
          class="flex size-7 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br from-emerald-400 to-teal-600 text-white"
          aria-hidden="true"
        >
          <Radio class="size-3.5" />
        </span>
        <RelayAvatar v-if="!isBroadcast" :name="peerName" size="sm" :online="peerOnline" />
        <p class="min-w-0 flex-1 truncate text-sm font-semibold">
          {{ peerName }}
          <span v-if="isBroadcast" class="ml-2 text-xs font-normal text-muted-foreground">
            发送给所有在线设备
          </span>
          <span v-else-if="isPhone" class="ml-2 text-xs font-normal text-muted-foreground">
            <span :class="peerOnline ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'">
              {{ peerOnline ? '在线' : '已离线' }}
            </span>
          </span>
          <span v-else class="ml-2 text-xs font-normal text-muted-foreground">
            <span class="font-mono">{{ peerIp }}</span>
            {{ ' · ' }}
            <span :class="peerOnline ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'">
              {{ peerOnline ? '在线' : '已离线' }}
            </span>
          </span>
        </p>

        <!-- 消息显示格式切换：气泡 ↔ 列表 -->
        <button
          :aria-label="store.chatStyle === 'bubble' ? '切换为列表显示' : '切换为气泡显示'"
          :aria-pressed="store.chatStyle === 'list'"
          :title="store.chatStyle === 'bubble' ? '切换为列表显示（紧凑单行）' : '切换为气泡显示'"
          class="text-muted-foreground hover:text-foreground flex size-8 shrink-0 items-center justify-center rounded-lg transition-colors hover:bg-accent"
          @click="switchChatStyle"
        >
          <ListOrdered v-if="store.chatStyle === 'bubble'" class="size-4" />
          <MessageSquare v-else class="size-4" />
        </button>
      </div>

      <!-- 消息流 -->
      <div class="min-h-0 flex-1">
        <MessageList :entries="entries" />
      </div>

      <!-- 输入区 -->
      <Composer v-if="peerOnline || isBroadcast" :peer="peer" :peer-name="peerName" />
      <div v-else class="shrink-0 border-t bg-card/60 p-4 text-center text-sm text-muted-foreground">
        对方已离线，暂时无法发送
      </div>

      <!-- 拖拽遮罩 -->
      <Transition name="fade">
        <div
          v-if="dragActive"
          class="pointer-events-none absolute inset-0 z-30 flex items-center justify-center bg-emerald-500/15 backdrop-blur-[2px]"
          role="status"
          aria-live="polite"
        >
          <div class="flex flex-col items-center gap-3 rounded-2xl border-2 border-dashed border-emerald-500/60 bg-card/95 px-8 py-6 shadow-xl">
            <span class="flex size-12 items-center justify-center rounded-xl bg-gradient-to-br from-emerald-400 to-teal-600 text-white shadow-lg shadow-emerald-500/30" aria-hidden="true">
              <UploadCloud class="size-6" />
            </span>
            <div class="text-center">
              <p class="text-sm font-semibold text-foreground">
                松手即发送给 {{ isBroadcast ? '所有人（需选择具体设备）' : peerName }}
              </p>
              <p class="mt-0.5 text-[11px] text-muted-foreground">
                {{ isBroadcast ? '广播会话暂不支持文件传输，请先在左侧选择设备' : '支持多文件，文件夹自动打包' }}
              </p>
            </div>
          </div>
        </div>
      </Transition>
    </template>
  </section>
</template>

<style scoped>
.fade-enter-active, .fade-leave-active { transition: opacity .15s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
