<script setup lang="ts">
import { Bird } from '@lucide/vue'
import { cn } from '../../lib/utils'

const props = defineProps<{ class?: string }>()
</script>

<template>
  <div
    :class="cn(
      'flex items-center justify-center rounded-xl bg-gradient-to-br from-emerald-400 via-emerald-500 to-teal-600 text-white shadow-sm shadow-emerald-500/30',
      'size-9',
      props.class
    )"
  >
    <Bird class="size-5" />
  </div>
</template>
