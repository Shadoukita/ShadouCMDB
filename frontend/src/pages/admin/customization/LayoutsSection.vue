<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { useCiList, type CiClass } from "../../../api/queries";
import type { UiSettingsDocument } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import { EDITOR_SUFFIX, openLayoutEditor } from "../../../lib/layoutEditor";
import ClassPicker from "./ClassPicker.vue";

/**
 * Customization › Detail and form layout: pick a class, then edit its layout
 * on a real CI page (the layout editor of CiDetailPage, in its own window):
 * by default the class's most recently updated CI, or another one found by
 * name. The editor saves a new settings version itself. A class without CIs
 * gets a link to the create form's layout editor. "Use the built-in layout"
 * drops the class's own layout from this page's draft (saved with the page).
 */
const props = defineProps<{ doc: UiSettingsDocument; error?: unknown }>();
const router = useRouter();
const cls = ref<CiClass>();
const own = computed(() => (cls.value ? props.doc.layouts.find((l) => l.classKey === cls.value!.key) : undefined));

/** The CIs offered: the most recently updated first, narrowed by the search. */
const search = ref("");
const q = ref("");
const PICKER_SIZE = 25;
const list = useCiList(
  () => ({ classId: cls.value?.id, limit: PICKER_SIZE, sort: "-updatedAt", ...(q.value ? { q: q.value } : {}) }),
  () => !!cls.value,
);
const found = computed(() => (list.isPlaceholderData.value ? undefined : list.data.value));
/** The CI chosen in the picker; until then the most recently updated one. */
const picked = ref<{ id: string; label: string } | null>(null);
const ci = computed(() => picked.value ?? (q.value ? null : (found.value?.data[0] ?? null)));
/** The class has no CI at all (not just none matching the search). */
const none = computed(() => !q.value && found.value?.page.total === 0);
watch(cls, () => {
  picked.value = null;
  search.value = "";
  q.value = "";
});
function onPick(id: string) {
  const c = found.value?.data.find((x) => x.id === id);
  if (c) picked.value = { id: c.id, label: c.label };
}

const createEditor = computed(() => (cls.value ? { path: `/cis/new${EDITOR_SUFFIX}`, query: { classId: cls.value.id } } : undefined));
function editCi() {
  if (cls.value && ci.value) openLayoutEditor(router, { path: `/cis/${ci.value.id}` }, cls.value.key);
}
function editOnCreate() {
  if (cls.value) openLayoutEditor(router, { path: "/cis/new", query: { classId: cls.value.id } }, cls.value.key);
}

const confirmBuiltIn = ref(false);
function useBuiltIn() {
  props.doc.layouts = props.doc.layouts.filter((l) => l !== own.value);
  confirmBuiltIn.value = false;
}
</script>

<template>
  <section class="panel">
    <div class="panel-header">
      <h2>{{ t("customization.layouts.title") }}</h2>
      <span class="muted">{{ t("customization.layouts.subtitle") }}</span>
    </div>
    <div class="panel-body layouts-body">
      <ClassPicker v-model:selected="cls" :customized="doc.layouts.map((l) => l.classKey)" noun="layout" />

      <template v-if="cls">
        <p class="muted" data-testid="layout-status">
          {{ own ? t("customization.layouts.own", { class: cls.name }) : t("customization.layouts.builtIn", { class: cls.name }) }}
        </p>

        <ErrorAlert v-if="list.isError.value" :error="list.error.value" :title="t('customization.layouts.listError')" :on-retry="() => list.refetch()" />
        <p v-else-if="none">
          <a :href="router.resolve(createEditor!).href" data-testid="layout-create-ci" @click.prevent="editOnCreate">{{ t("customization.layouts.createCi", { class: cls.name }) }}</a>
        </p>
        <div v-else class="layouts-edit">
          <div class="inline-control">
            <label for="layout-ci-search">{{ t("customization.layouts.otherCi") }}</label>
            <input
              id="layout-ci-search"
              v-model="search"
              type="search"
              :placeholder="t('customization.layouts.searchPlaceholder')"
              @keydown.enter.prevent="q = search.trim()"
              @search="q = search.trim()"
            />
            <label class="sr-only" for="layout-ci">{{ t("customization.layouts.ci") }}</label>
            <select id="layout-ci" :value="ci?.id ?? ''" :disabled="!found || found.data.length === 0" @change="onPick(($event.target as HTMLSelectElement).value)">
              <option v-if="!ci" value="" disabled>{{ found && found.data.length === 0 ? t("customization.layouts.noMatch") : t("customization.layouts.chooseCi") }}</option>
              <option v-if="ci && !found?.data.some((c) => c.id === ci!.id)" :value="ci.id">{{ ci.label }}</option>
              <option v-for="c in found?.data ?? []" :key="c.id" :value="c.id">{{ c.label }}</option>
            </select>
            <span class="muted">{{ t("customization.layouts.recentHint", { n: PICKER_SIZE }) }}</span>
          </div>
          <span>
            <button type="button" class="btn btn-primary" :disabled="!ci" data-testid="layout-edit-ci" @click="editCi">
              {{ ci ? t("customization.layouts.editCi", { name: ci.label }) : t("customization.layouts.editCiLoading") }}
            </button>
          </span>
          <p class="hint">{{ t("customization.layouts.editHint", { class: cls.name }) }}</p>
        </div>

        <div v-if="own" class="layouts-reset">
          <button type="button" class="btn btn-sm" @click="confirmBuiltIn = true">{{ t("customization.layouts.useBuiltIn", { class: cls.name }) }}</button>
        </div>
      </template>
      <p v-else class="muted">{{ t("customization.layouts.chooseClass") }}</p>
    </div>
  </section>

  <ConfirmDialog
    :open="confirmBuiltIn"
    :title="t('customization.layouts.useBuiltInTitle', { class: cls?.name ?? '' })"
    :confirm-label="t('customization.layouts.useBuiltInConfirm')"
    @confirm="useBuiltIn"
    @cancel="confirmBuiltIn = false"
  >
    {{ t("customization.layouts.useBuiltInBody", { class: cls?.name ?? "" }) }}
  </ConfirmDialog>
</template>

<style scoped>
.layouts-body,
.layouts-edit {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
}
.layouts-body p {
  margin: 0;
}
.layouts-edit input[type="search"] {
  max-width: 260px;
}
.layouts-edit select {
  max-width: 360px;
}
</style>
