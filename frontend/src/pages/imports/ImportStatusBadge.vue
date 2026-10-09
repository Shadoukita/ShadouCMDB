<script setup lang="ts">
import { computed } from "vue";
import type { ImportStatus } from "../../api/imports";
import { STATUS } from "../../lib/imports";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";

/** An import's status as icon plus text, never colour alone. */
const props = defineProps<{ status: ImportStatus }>();
const s = computed(() => STATUS[props.status]);
</script>

<template>
  <span :class="['badge', s.tone]"><Icon :name="s.icon" :size="14" /> {{ t(s.label) }}</span>
</template>
