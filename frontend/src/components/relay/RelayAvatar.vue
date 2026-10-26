<script setup lang="ts">
import { gradientForName, avatarInitial } from '../../lib/relay-format'
import { cn } from '../../lib/utils'

const props = withDefaults(
  defineProps<{
    name: string
    size?: 'sm' | 'md' | 'lg'
    online?: boolean | null
  }>(),
  { size: 'md', online: null }
)

const sizeCls =
  props.size === 'sm' ? 'size-8 text-xs' : props.size === 'lg' ? 'size-12 text-base' : 'size-10 text-sm'
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
      v-if="online != null"
      :class="cn(
        'absolute right-0 bottom-0 size-2.5 rounded-full border-2 border-background',
        online ? 'bg-emerald-500' : 'bg-muted-foreground/40'
      )"
    />
  </div>
</template>
