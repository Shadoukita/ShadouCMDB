<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { UiLayoutTemplate } from "../../../api/uiSettings";
import FormDialog from "../../../components/FormDialog.vue";
import { t } from "../../../i18n";
import { templateNameProblem, TEMPLATE_DESCRIPTION_MAX, TEMPLATE_NAME_MAX } from "../../../lib/layoutTemplates";

/**
 * New, duplicated or renamed layout template (Customization › Layouts): name
 * (unique among the templates, ignoring case), description and, for a new one,
 * what it starts from: blank (the built-in layout) or a copy of another template.
 */
const props = defineProps<{
  open: boolean;
  mode: "new" | "rename";
  templates: readonly UiLayoutTemplate[];
  /** Renamed: the template; new: the starting values (a duplicate starts from its template). */
  initial: { key?: string; name: string; description?: string; from?: string };
}>();
const emit = defineEmits<{ submit: [v: { name: string; description: string; from: string }]; cancel: [] }>();

const name = ref("");
const description = ref("");
const from = ref("");
const touched = ref(false);
watch(
  () => props.open,
  (open) => {
    if (!open) return;
    name.value = props.initial.name;
    description.value = props.initial.description ?? "";
    from.value = props.initial.from ?? "";
    touched.value = false;
  },
  { immediate: true },
);
const problem = computed(() => templateNameProblem(name.value, props.templates, props.mode === "rename" ? props.initial.key : undefined));
const error = computed(() => (touched.value && problem.value ? t(`layoutTemplates.name.${problem.value}`, { max: TEMPLATE_NAME_MAX }) : ""));

async function submit() {
  touched.value = true;
  if (problem.value) {
    await nextTick();
    document.getElementById("tpl-name")?.focus();
    return;
  }
  emit("submit", { name: name.value.trim(), description: description.value.trim(), from: from.value });
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="mode === 'rename' ? t('layoutTemplates.renameTitle', { name: initial.name }) : t('layoutTemplates.newTitle')"
    :submit-label="mode === 'rename' ? t('layoutTemplates.renameConfirm') : t('layoutTemplates.newConfirm')"
    @submit="submit"
    @cancel="emit('cancel')"
  >
    <div class="stack">
      <div class="field">
        <label for="tpl-name">{{ t("layoutTemplates.name") }}<span class="req" aria-hidden="true">*</span></label>
        <input
          id="tpl-name"
          v-model="name"
          type="text"
          required
          autocomplete="off"
          :maxlength="TEMPLATE_NAME_MAX"
          :aria-invalid="!!error"
          :aria-describedby="error ? 'tpl-name-error' : undefined"
          @blur="touched = true"
        />
        <span v-if="error" id="tpl-name-error" class="error">{{ error }}</span>
      </div>
      <div class="field">
        <label for="tpl-description">{{ t("layoutTemplates.description") }}</label>
        <input id="tpl-description" v-model="description" type="text" autocomplete="off" :maxlength="TEMPLATE_DESCRIPTION_MAX" />
      </div>
      <div v-if="mode === 'new'" class="field">
        <label for="tpl-from">{{ t("layoutTemplates.startFrom") }}</label>
        <select id="tpl-from" v-model="from">
          <option value="">{{ t("layoutTemplates.blank") }}</option>
          <option v-for="tp in templates" :key="tp.key" :value="tp.key">{{ t("layoutTemplates.copyOf", { name: tp.name }) }}</option>
        </select>
      </div>
      <p class="hint">{{ mode === "rename" ? t("layoutTemplates.renameHint") : t("layoutTemplates.newHint") }}</p>
    </div>
  </FormDialog>
</template>
