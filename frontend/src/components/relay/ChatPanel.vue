<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import QRCode from 'qrcode'
import { CloudUpload, Radio } from '@lucide/vue'
import RelayAvatar from './RelayAvatar.vue'
import MessageList from './MessageList.vue'
import Composer from './Composer.vue'
import { store, filterEntriesForPeer } from '../../stores/relay-store'
import { relayActions, dragState } from '../../hooks/use-relay'

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

/* 电脑：断开连接 */
function disconnectPc() {
  if (!isPhone.value) relayActions.disconnectPeer(props.peer)
}

/* 拖拽遮罩状态（由 use-relay 的全局拖拽事件驱动） */
const dragActive = computed(() => dragState.active && store.selectedPeer === props.peer)
</script>

<template>
  <section class="relative flex min-h-0 min-w-0 flex-1 flex-col bg-card/40">
    <!-- 手机未配对：扫码配对视图 -->
    <template v-if="isPhone && !store.phoneConnected">
      <div class="flex h-14 shrink-0 items-center gap-3 border-b bg-card/70 px-4">
        <div class="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground">
          <span class="text-sm font-semibold text-muted-foreground">?</span>
        </div>
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-semibold">手机</p>
          <p class="truncate text-xs text-muted-foreground">扫码配对后即可互传文件与文本</p>
        </div>
      </div>
      <div class="custom-scroll flex-1 overflow-y-auto p-4">
        <div v-if="store.pairPending" class="mx-auto max-w-sm rounded-2xl border border-emerald-500/40 bg-emerald-500/5 p-5 text-center shadow-sm">
          <p class="text-sm font-medium">手机「{{ store.pairPending.device }}」请求配对</p>
          <p class="mt-1 text-xs text-muted-foreground">电脑无需操作：把配对码输入到手机即可</p>
          <div class="my-3 font-mono text-4xl font-bold tracking-[8px] text-emerald-700">
            {{ store.pairPending.code }}
          </div>
        </div>
        <div class="mx-auto mt-4 flex max-w-sm flex-col items-center gap-2 rounded-2xl border bg-card p-4 shadow-sm">
          <img v-if="qrData" :src="qrData" alt="扫码连接" width="240" />
          <p class="text-xs text-muted-foreground">手机扫码 · 浏览器打开 · 免安装</p>
        </div>
      </div>
    </template>

    <!-- 正常会话 -->
    <template v-else>
      <!-- 会话标题栏 -->
      <div class="flex h-14 shrink-0 items-center gap-3 border-b bg-card/70 px-3 backdrop-blur-sm sm:px-4">
        <div
          v-if="isBroadcast"
          class="flex size-10 shrink-0 items-center justify-center rounded-full bg-gradient-to-br from-emerald-400 to-teal-600 text-white"
        >
          <Radio class="size-5" />
        </div>
        <RelayAvatar v-if="!isBroadcast" :name="peerName" :online="peerOnline" />
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-semibold">{{ peerName }}</p>
          <p class="truncate text-xs text-muted-foreground">
            <template v-if="isBroadcast">消息将发送给所有在线电脑</template>
            <template v-else>
              <span v-if="isPhone" class="font-mono">{{ store.serverAddr }}</span>
              <span v-else class="font-mono">{{ peerIp }}</span>
              {{ ' · ' }}
              <span :class="peerOnline ? 'text-emerald-600' : 'text-muted-foreground'">
                {{ peerOnline ? '在线' : '已离线' }}
              </span>
            </template>
          </p>
        </div>
        <button
          v-if="!isPhone"
          class="rounded-lg border px-3 py-1.5 text-xs transition-colors hover:bg-accent"
          @click="disconnectPc"
        >
          断开
        </button>
      </div>

      <!-- 消息流 -->
      <div class="min-h-0 flex-1">
        <MessageList :peer="peer" />
      </div>

      <!-- 输入区 -->
      <Composer v-if="peerOnline" :peer="peer" />
      <div v-else class="shrink-0 border-t bg-card/60 p-4 text-center text-sm text-muted-foreground">
        对方已离线，暂时无法发送
      </div>

      <!-- 拖拽遮罩 -->
      <Transition name="fade">
        <div
          v-if="dragActive"
          class="pointer-events-none absolute inset-0 z-30 flex items-center justify-center bg-emerald-500/20 backdrop-blur-[2px]"
        >
          <div class="flex flex-col items-center gap-4 rounded-3xl border-2 border-dashed border-emerald-500/60 bg-card/90 px-10 py-8 shadow-xl">
            <span class="flex size-16 items-center justify-center rounded-2xl bg-gradient-to-br from-emerald-400 to-teal-600 text-white shadow-lg shadow-emerald-500/30">
              <CloudUpload class="size-8" />
            </span>
            <div class="text-center">
              <p class="text-base font-semibold">松手即发送给 {{ peerName }}</p>
              <p class="mt-1 text-xs text-muted-foreground">支持多文件，文件夹将自动打包</p>
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
