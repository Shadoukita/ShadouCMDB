<script setup lang="ts">
import { computed, nextTick, ref } from "vue";

/**
 * A write-only secret (OIDC client secret, LDAP bind password). The API never returns it, only
 * whether one is stored. The model follows the PATCH rules: undefined keeps the stored secret,
 * a string replaces it, null removes it. Required (the server address changed): the stored secret
 * cannot be kept, so the parent sets an empty string and "Keep stored" is not offered.
 */
const model = defineModel<string | null | undefined>({ required: true });
const props = defineProps<{ id: string; label: string; isSet: boolean; error?: string; hint?: string; removable?: boolean; required?: boolean }>();

const input = ref<HTMLInputElement>();
/** No secret stored yet, or the administrator chose to replace it: show the input. */
const editing = computed(() => !props.isSet || typeof model.value === "string");
const describedBy = computed(
  () => [props.error ? `${props.id}-err` : "", props.hint ? `${props.id}-hint` : "", `${props.id}-state`].filter(Boolean).join(" "),
);

async function replace() {
  model.value = "";
  await nextTick();
  input.value?.focus();
}
</script>

<template>
  <div class="field">
    <label v-if="editing" :for="id">{{ label }}<span v-if="required" class="req" aria-hidden="true">*</span></label>
    <span v-else :id="`${id}-label`" class="label">{{ label }}</span>
    <div v-if="editing" class="secret-row">
      <input
        :id="id"
        ref="input"
        :value="model ?? ''"
        type="password"
        autocomplete="new-password"
        spellcheck="false"
        :aria-invalid="!!error"
        :aria-required="required || undefined"
        :aria-describedby="describedBy"
        @input="model = ($event.target as HTMLInputElement).value"
      />
      <button v-if="isSet && !required" type="button" class="btn" @click="model = undefined">Keep stored</button>
    </div>
    <div v-else class="secret-row">
      <span :id="`${id}-state`" class="secret-state" :class="{ removed: model === null }">
        {{ model === null ? "Will be removed when you save" : "Stored — never shown" }}
      </span>
      <template v-if="model === null">
        <button type="button" class="btn" :aria-describedby="`${id}-label`" @click="model = undefined">Undo</button>
      </template>
      <template v-else>
        <button type="button" class="btn" :aria-describedby="`${id}-label`" @click="replace">Replace…</button>
        <button v-if="removable" type="button" class="btn btn-quiet-danger" :aria-describedby="`${id}-label`" @click="model = null">Remove</button>
      </template>
    </div>
    <span v-if="editing" :id="`${id}-state`" class="sr-only">{{ isSet ? "Replaces the stored secret when you save." : "No secret stored." }}</span>
    <span v-if="error" :id="`${id}-err`" class="error">{{ error }}</span>
    <span v-if="hint" :id="`${id}-hint`" class="hint">{{ hint }}</span>
  </div>
</template>

<style scoped>
.secret-row {
  display: flex;
  gap: var(--sp-2);
  align-items: center;
}
.secret-row input {
  flex: 1;
}
.secret-state {
  flex: 1;
  font-size: var(--fs-sm);
  color: var(--c-text-muted);
}
.secret-state.removed {
  color: var(--c-danger-text);
}
</style>
