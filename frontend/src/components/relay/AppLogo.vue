<script setup lang="ts">
/**
 * 品牌 Logo：飞鸽（Bird 图标 + emerald 渐变圆角底），与应用图标同款设计
 */
import { Bird } from '@lucide/vue'
import { computed } from 'vue'
import { cn } from '../../lib/utils'

const props = withDefaults(defineProps<{ size?: 'sm' | 'md' }>(), { size: 'md' })
const cls = computed(() =>
  props.size === 'sm'
    ? 'size-7 rounded-lg'
    : 'size-9 rounded-xl'
)
</script>

<template>
  <div
    :class="cn(
      'flex items-center justify-center bg-gradient-to-br from-emerald-400 via-emerald-500 to-teal-600 text-white shadow-sm shadow-emerald-500/30',
      cls
    )"
  >
    <Bird :class="props.size === 'sm' ? 'size-4' : 'size-5'" stroke-width="2.2" />
  </div>
</template>
