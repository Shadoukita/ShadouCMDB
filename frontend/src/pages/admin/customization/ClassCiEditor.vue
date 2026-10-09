<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { useCiList, type CiClass } from "../../../api/queries";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import { EDITOR_SUFFIX, openLayoutEditor, TEMPLATE_QUERY } from "../../../lib/layoutEditor";

/**
 * Customization › Layouts, one class: edit its default template on a real CI
 * page (the layout editor of CiDetailPage, in its own window): by default the
 * class's most recently updated CI, or another one found by name. The editor
 * saves a new settings version itself. A class without CIs gets a link to the
 * create form's layout editor.
 */
const props = defineProps<{
  cls: CiClass;
  /** The class's default template, when it is saved (the editor loads the saved settings). */
  templateKey?: string;
  templateName: string;
  /** Whether it is known which templates are saved (until then the editor would not know which one to open). */
  ready: boolean;
}>();
const emit = defineEmits<{ close: [] }>();
const router = useRouter();

/** The CIs offered: the most recently updated first, narrowed by the search. */
const search = ref("");
const q = ref("");
let typing: ReturnType<typeof setTimeout> | undefined;
watch(search, (v) => {
  clearTimeout(typing);
  typing = setTimeout(() => (q.value = v.trim()), 300);
});
onBeforeUnmount(() => clearTimeout(typing));
const PICKER_SIZE = 25;
const list = useCiList(() => ({ classId: props.cls.id, limit: PICKER_SIZE, sort: "-updatedAt", ...(q.value ? { q: q.value } : {}) }));
const found = computed(() => (list.isPlaceholderData.value ? undefined : list.data.value));
/** The CI chosen in the picker; until then the most recently updated one. */
const picked = ref<{ id: string; label: string } | null>(null);
const ci = computed(() => picked.value ?? (q.value ? null : (found.value?.data[0] ?? null)));
/** The class has no CI at all (not just none matching the search). */
const none = computed(() => !q.value && found.value?.page.total === 0);
watch(
  () => props.cls.key,
  () => {
    picked.value = null;
    search.value = "";
    clearTimeout(typing);
    q.value = "";
  },
);
function onPick(id: string) {
  const c = found.value?.data.find((x) => x.id === id);
  if (c) picked.value = { id: c.id, label: c.label };
}

const templateQuery = computed(() => (props.templateKey ? { [TEMPLATE_QUERY]: props.templateKey } : {}));
const createEditor = computed(() => ({ path: `/cis/new${EDITOR_SUFFIX}`, query: { classId: props.cls.id, ...templateQuery.value } }));
function editCi() {
  if (ci.value && props.ready) openLayoutEditor(router, { path: `/cis/${ci.value.id}` }, props.cls.key, props.templateKey);
}
function editOnCreate() {
  if (props.ready) openLayoutEditor(router, { path: "/cis/new", query: { classId: props.cls.id } }, props.cls.key, props.templateKey);
}
</script>

<template>
  <section class="panel class-ci-editor" :aria-label="t('customization.layouts.editPanel', { class: cls.name })" data-testid="layout-class-panel">
    <div class="panel-header">
      <h3>{{ t("customization.layouts.editPanel", { class: cls.name }) }}</h3>
      <button type="button" class="btn btn-sm" @click="emit('close')">{{ t("customization.layouts.closePanel") }}</button>
    </div>
    <div class="panel-body layouts-edit">
      <p class="muted" data-testid="layout-status">{{ t("customization.layouts.usesTemplate", { class: cls.name, name: templateName }) }}</p>
      <ErrorAlert v-if="list.isError.value" :error="list.error.value" :title="t('customization.layouts.listError')" :on-retry="() => list.refetch()" />
      <p v-else-if="none">
        <a :href="router.resolve(createEditor).href" data-testid="layout-create-ci" @click.prevent="editOnCreate">{{ t("customization.layouts.createCi", { class: cls.name }) }}</a>
      </p>
      <template v-else>
        <div class="inline-control">
          <label for="layout-ci-search">{{ t("customization.layouts.otherCi") }}</label>
          <input id="layout-ci-search" v-model="search" type="search" :placeholder="t('customization.layouts.searchPlaceholder')" @keydown.enter.prevent />
          <label class="sr-only" for="layout-ci">{{ t("customization.layouts.ci") }}</label>
          <select id="layout-ci" :value="ci?.id ?? ''" :disabled="!found || found.data.length === 0" @change="onPick(($event.target as HTMLSelectElement).value)">
            <option v-if="!ci" value="" disabled>{{ found && found.data.length === 0 ? t("customization.layouts.noMatch") : t("customization.layouts.chooseCi") }}</option>
            <option v-if="ci && !found?.data.some((c) => c.id === ci!.id)" :value="ci.id">{{ ci.label }}</option>
            <option v-for="c in found?.data ?? []" :key="c.id" :value="c.id">{{ c.label }}</option>
          </select>
          <span class="muted">{{ t("customization.layouts.recentHint", { n: PICKER_SIZE }) }}</span>
        </div>
        <span>
          <button type="button" class="btn btn-primary" :disabled="!ci || !ready" data-testid="layout-edit-ci" @click="editCi">
            {{ ci ? t("customization.layouts.editCi", { name: ci.label }) : t("customization.layouts.editCiLoading") }}
          </button>
        </span>
        <p class="hint">{{ t("customization.layouts.editHint", { name: templateName }) }}</p>
      </template>
    </div>
  </section>
</template>

<style scoped>
.layouts-edit {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.layouts-edit p {
  margin: 0;
}
.layouts-edit input[type="search"] {
  max-width: 260px;
}
.layouts-edit select {
  max-width: 360px;
}
</style>
