<script setup lang="ts">
import { computed } from "vue";
import { ciStateParts, type CiStateInput } from "../lib/ciState";

/**
 * "Deleted" or "Inactive" (outside its validity period) badge of a CI; nothing for an active one
 * unless `showActive` is set. A validity period that starts or ends in the future says when
 * ("Activates on …", "Deactivates on …") when the CI's validFrom/validUntil are given.
 * The texts come from the message catalog (lib/ciState).
 */
const props = defineProps<{ ci: CiStateInput; showActive?: boolean }>();
const parts = computed(() => ciStateParts(props.ci, props.showActive));
</script>

<template>
  <span v-for="(p, i) in parts" :key="i" :class="p.tone === 'muted' ? 'muted' : ['badge', p.tone]" :title="p.title">{{ p.text }}</span>
</template>
