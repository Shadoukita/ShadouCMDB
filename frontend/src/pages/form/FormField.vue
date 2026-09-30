<script setup lang="ts">
import { computed, onMounted, onUpdated, ref } from "vue";

/**
 * Label + control + error + hint. The control comes from the default slot, which
 * receives the ids to wire up (`id`, `aria-invalid`, `aria-describedby`).
 * A required field marks its control `aria-required` itself; the visible asterisk is hidden
 * from assistive technology, so the field is announced as "Name, required", not "Name star".
 */
const props = defineProps<{ id: string; label: string; required?: boolean; error?: string; hint?: string; wide?: boolean }>();
const describedBy = computed(
  () => [props.error ? `${props.id}-err` : "", props.hint ? `${props.id}-hint` : ""].filter(Boolean).join(" ") || undefined,
);

const root = ref<HTMLElement>();
function markRequired() {
  const control = root.value?.querySelector(`#${CSS.escape(props.id)}`);
  if (!control) return;
  if (props.required) control.setAttribute("aria-required", "true");
  else control.removeAttribute("aria-required");
}
onMounted(markRequired);
onUpdated(markRequired);
</script>

<template>
  <div ref="root" :class="['field', { wide }]">
    <label :for="id">
      {{ label }}<span v-if="required" class="req" aria-hidden="true">*</span>
    </label>
    <slot :id="id" :invalid="!!error" :described-by="describedBy" />
    <span v-if="error" :id="`${id}-err`" class="error">{{ error }}</span>
    <span v-if="hint" :id="`${id}-hint`" class="hint" dir="auto">{{ hint }}</span>
  </div>
</template>
