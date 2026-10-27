<script setup lang="ts">
/**
 * UnreadBadge：会话未读数字角标（红底白字，IM 惯例）
 * 超过 99 显示 99+；位置固定在列表行尾，替代原有的状态/IP 文案
 */
defineProps<{ count: number }>()
</script>

<template>
  <span
    class="flex h-[18px] min-w-[18px] shrink-0 items-center justify-center rounded-full bg-red-500 px-1 text-[10px] font-semibold leading-none text-white"
    :aria-label="`${count} 条未读消息`"
  >
    {{ count > 99 ? '99+' : count }}
  </span>
</template>
