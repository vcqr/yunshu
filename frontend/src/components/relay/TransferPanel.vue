<script setup lang="ts">
import { computed, ref } from 'vue'
import { Inbox } from '@lucide/vue'
import TransferCard from './TransferCard.vue'
import { splitTransfers } from '../../stores/relay-store'
import { cn } from '../../lib/utils'

const filterDone = defineModel<boolean>('filterDone', { default: false })
const emit = defineEmits<{ (e: 'update:filterDone', v: boolean): void }>()

const active = computed(() => splitTransfers().active)
const finished = computed(() => splitTransfers().finished)
const rows = computed(() => (filterDone.value ? finished.value : active.value))
void ref
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <div class="shrink-0 px-1 pb-2">
      <p class="text-sm font-semibold">传输任务</p>
    </div>
    <div class="flex shrink-0 gap-1.5 px-1 pb-2">
      <button
        :class="cn(
          'rounded-full px-3 py-1.5 text-xs font-medium transition-colors',
          !filterDone ? 'bg-emerald-500/15 text-emerald-600' : 'text-muted-foreground hover:bg-accent'
        )"
        @click="emit('update:filterDone', false)"
      >
        进行中 {{ active.length }}
      </button>
      <button
        :class="cn(
          'rounded-full px-3 py-1.5 text-xs font-medium transition-colors',
          filterDone ? 'bg-emerald-500/15 text-emerald-600' : 'text-muted-foreground hover:bg-accent'
        )"
        @click="emit('update:filterDone', true)"
      >
        已完成 {{ finished.length }}
      </button>
    </div>
    <div class="custom-scroll min-h-0 flex-1 overflow-y-auto px-1">
      <div class="space-y-2">
        <TransferCard v-for="t in rows" :key="t.id" :transfer="t" />
        <div v-if="!rows.length" class="flex flex-col items-center gap-3 py-14 text-center">
          <div class="text-muted-foreground"><Inbox class="size-9" /></div>
          <div class="text-xs text-muted-foreground">
            {{ filterDone ? '暂无已完成任务' : '暂无进行中的任务' }}
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
