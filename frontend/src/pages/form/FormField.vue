<script setup lang="ts">
import { computed } from "vue";

/**
 * Label + control + error + hint. The control comes from the default slot, which
 * receives the ids to wire up (`id`, `aria-invalid`, `aria-describedby`).
 */
const props = defineProps<{ id: string; label: string; required?: boolean; error?: string; hint?: string; wide?: boolean }>();
const describedBy = computed(
  () => [props.error ? `${props.id}-err` : "", props.hint ? `${props.id}-hint` : ""].filter(Boolean).join(" ") || undefined,
);
</script>

<template>
  <div :class="['field', { wide }]">
    <label :for="id">
      {{ label }}<span v-if="required" class="req" aria-label="required">*</span>
    </label>
    <slot :id="id" :invalid="!!error" :described-by="describedBy" />
    <span v-if="error" :id="`${id}-err`" class="error">{{ error }}</span>
    <span v-if="hint" :id="`${id}-hint`" class="hint">{{ hint }}</span>
  </div>
</template>
