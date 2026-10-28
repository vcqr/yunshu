<script setup lang="ts">
import { computed } from 'vue'
import { gradientForName, avatarInitial } from '../../lib/relay-format'
import { cn } from '../../lib/utils'
import type { Presence } from '../../lib/relay-types'

const props = withDefaults(
  defineProps<{
    name: string
    size?: 'sm' | 'md' | 'lg'
    /** 右下角状态灯；不传不显示（消息流里的发送者头像等场景） */
    presence?: Presence | null
  }>(),
  { size: 'md', presence: null }
)

const sizeCls =
  props.size === 'sm' ? 'size-7 text-[10px]' : props.size === 'lg' ? 'size-12 text-base' : 'size-10 text-sm'

const presenceLabel = computed(
  () =>
    ({ online: '在线', stealth: '隐身', dnd: '免打扰', offline: '离线' } as const)[
      props.presence ?? 'online'
    ]
)
</script>

<template>
  <div class="relative shrink-0">
    <div
      :class="cn(
        'flex items-center justify-center rounded-full bg-gradient-to-br font-semibold text-white select-none',
        gradientForName(name),
        sizeCls
      )"
    >
      {{ avatarInitial(name) }}
    </div>
    <span
      v-if="presence"
      role="img"
      :aria-label="`状态：${presenceLabel}`"
      :title="presenceLabel"
      :class="cn(
        'absolute right-0 bottom-0 size-2.5 rounded-full border-2 border-background',
        presence === 'online' ? 'bg-emerald-500'
          : presence === 'stealth' ? 'bg-amber-400'
          : presence === 'dnd' ? 'bg-violet-500'
          : 'bg-muted-foreground/40'
      )"
    />
  </div>
</template>
