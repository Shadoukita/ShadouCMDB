<script setup lang="ts">
import { computed } from "vue";
import type { ImportStatus } from "../../api/imports";
import { STATUS } from "../../lib/imports";

/** An import's status as icon plus text, never colour alone. */
const props = defineProps<{ status: ImportStatus }>();
const s = computed(() => STATUS[props.status]);
</script>

<template>
  <span :class="['badge', s.tone]"><span aria-hidden="true">{{ s.icon }}</span> {{ s.label }}</span>
</template>
