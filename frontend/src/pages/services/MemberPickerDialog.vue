<script setup lang="ts">
import { computed, nextTick, onMounted, ref, useId, watch } from "vue";
import { ApiError } from "../../api/client";
import { useCiClasses, useCiList } from "../../api/queries";
import { useAddMembers, useMembershipOf } from "../../api/services";
import CiStateBadge from "../../components/CiStateBadge.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useDebounced } from "../../lib/composables";
import { pickerErrors, type PickerError } from "../../lib/serviceMembers";
import { useSessionStore } from "../../stores/session";

/**
 * "Add members" (spec SHAA-927 §5.5): a modal over the server-side CI search, paged at 50. After each page loads,
 * one `…/members?ciId=` call marks the CIs that already are members. The "Selected (N)" tray keeps the choice across
 * pages and searches, up to `maxBatch`. The server decides what can be added: on a refusal the dialog stays open,
 * a focused summary counts the failures, and each failing CI in the tray says why.
 */
const props = defineProps<{
  service: { id: string; name: string };
  limits: { maxBatch: number; maxMembers: number; maxNesting: number };
}>();
const emit = defineEmits<{ close: []; added: [added: number, already: number] }>();

const PAGE = 50;
const uid = useId();
const titleId = `picker-title-${uid}`;
const dialog = ref<HTMLDialogElement>();
const searchInput = ref<HTMLInputElement>();
onMounted(() => {
  dialog.value?.showModal();
  searchInput.value?.focus();
});

const session = useSessionStore();
const classes = useCiClasses();
const classChoices = computed(() =>
  (classes.data.value ?? []).filter((c) => !c.isAbstract && session.canOnClass(c.id, "view")).sort((a, b) => a.name.localeCompare(b.name)),
);

// ---------- Search ----------
const qText = ref("");
const q = useDebounced(qText, 300);
const classId = ref("");
const offset = ref(0);
watch([q, classId], () => (offset.value = 0));
const results = useCiList(() => ({
  limit: PAGE,
  offset: offset.value,
  sort: "label",
  ...(q.value.trim() ? { q: q.value.trim() } : {}),
  ...(classId.value ? { classId: classId.value } : {}),
}));
const rows = computed(() => results.data.value?.data ?? []);
const total = computed(() => results.data.value?.page.total ?? 0);
const pageIds = computed(() => rows.value.map((c) => c.id));
const membership = useMembershipOf(() => props.service.id, pageIds);
const isMember = (id: string) => !!membership.data.value?.has(id);

// ---------- Selection ----------
interface Picked {
  id: string;
  label: string;
  ident: string;
  className: string;
}
const selected = ref(new Map<string, Picked>());
const full = computed(() => selected.value.size >= props.limits.maxBatch);
const errors = ref(new Map<string, PickerError>());
const generalErrors = ref<PickerError[]>([]);
const otherError = ref<unknown>(null);

function pick(c: { id: string; label: string; ident: string; class: { name: string } }, on: boolean) {
  const next = new Map(selected.value);
  if (on && !full.value) next.set(c.id, { id: c.id, label: c.label, ident: c.ident, className: c.class.name });
  else if (!on) next.delete(c.id);
  selected.value = next;
}
function unpick(id: string) {
  const next = new Map(selected.value);
  next.delete(id);
  selected.value = next;
  if (errors.value.delete(id)) errors.value = new Map(errors.value);
}
const addable = computed(() => rows.value.filter((c) => !isMember(c.id)));
const pageAllPicked = computed(() => addable.value.length > 0 && addable.value.every((c) => selected.value.has(c.id)));
function pickPage(on: boolean) {
  const next = new Map(selected.value);
  for (const c of addable.value) {
    if (!on) next.delete(c.id);
    else if (next.size < props.limits.maxBatch) next.set(c.id, { id: c.id, label: c.label, ident: c.ident, className: c.class.name });
  }
  selected.value = next;
}

// ---------- Submit ----------
const add = useAddMembers(() => props.service.id);
const summary = ref<HTMLElement>();
const failedCount = computed(() => errors.value.size);
async function submit() {
  if (selected.value.size === 0 || add.isPending.value) return;
  const ids = [...selected.value.keys()];
  errors.value = new Map();
  generalErrors.value = [];
  otherError.value = null;
  try {
    const res = await add.mutateAsync(ids);
    emit("added", res.added.length, res.alreadyMembers.length);
  } catch (e) {
    if (e instanceof ApiError && e.code === "VALIDATION_ERROR" && e.details.length > 0) {
      const found = pickerErrors(e.details, ids, (id) => selected.value.get(id)?.label ?? id, props.limits);
      errors.value = found.byId;
      generalErrors.value = found.general;
    } else {
      otherError.value = e;
    }
    await nextTick();
    summary.value?.focus();
  }
}

// ---------- Close ----------
const discarding = ref(false);
function requestClose() {
  if (add.isPending.value) return;
  if (selected.value.size > 0) discarding.value = true;
  else close();
}
function close() {
  discarding.value = false;
  dialog.value?.close();
  emit("close");
}
function onCancel(e: Event) {
  // Esc: ask first when something is selected.
  e.preventDefault();
  if (!discarding.value) requestClose();
}
/** Moves focus to a failing CI in the tray (the summary's links). */
function focusPicked(id: string) {
  document.getElementById(`pick-${uid}-${id}`)?.focus();
}
const from = computed(() => (total.value === 0 ? 0 : offset.value + 1));
const to = computed(() => Math.min(offset.value + PAGE, total.value));
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm form-dialog wide member-picker" role="dialog" aria-modal="true" :aria-labelledby="titleId" @cancel="onCancel">
      <form novalidate @submit.prevent="submit">
        <h2 :id="titleId">{{ t("services.picker.title", { service: service.name }) }}</h2>
        <div class="body">
          <div v-if="failedCount > 0 || generalErrors.length > 0 || otherError" ref="summary" class="picker-summary" tabindex="-1" role="alert">
            <div v-if="failedCount > 0 || generalErrors.length > 0" class="alert alert-error">
              <strong v-if="failedCount > 0">{{ t("services.picker.failedSummary", { n: failedCount }) }}</strong>
              <ul>
                <li v-for="g in generalErrors" :key="g.code + g.message">{{ g.message }}</li>
                <li v-for="[id, err] in errors" :key="id">
                  <a :href="`#pick-${uid}-${id}`" @click.prevent="focusPicked(id)">{{ selected.get(id)?.label ?? id }}</a>: {{ err.message }}
                </li>
              </ul>
            </div>
            <ErrorAlert v-else :error="otherError" :title="t('services.picker.failed')" />
          </div>

          <div class="toolbar picker-toolbar" role="search">
            <div class="field search">
              <label :for="`picker-q-${uid}`">{{ t("services.picker.search") }}</label>
              <input :id="`picker-q-${uid}`" ref="searchInput" v-model="qText" type="search" maxlength="200" :placeholder="t('services.members.searchPlaceholder')" />
            </div>
            <div class="field">
              <label :for="`picker-class-${uid}`">{{ t("services.members.class") }}</label>
              <select :id="`picker-class-${uid}`" v-model="classId">
                <option value="">{{ t("services.members.allClasses") }}</option>
                <option v-for="c in classChoices" :key="c.id" :value="c.id">{{ c.name }}</option>
              </select>
            </div>
          </div>

          <ErrorAlert v-if="results.isError.value" :error="results.error.value" :title="t('services.picker.searchFailed')" :on-retry="() => results.refetch()" />
          <p v-else-if="results.isPending.value" class="muted" role="status"><span class="spinner" aria-hidden="true" /> {{ t("services.picker.loading") }}</p>
          <p v-else-if="total === 0" class="muted" role="status">{{ t("services.picker.noResults") }}</p>
          <template v-else>
            <p v-if="membership.isError.value" class="muted">{{ t("services.picker.alreadyCheckFailed") }}</p>
            <div class="table-wrap" role="region" tabindex="0" :aria-label="t('services.picker.results')">
              <table :class="['data', { loading: results.isPlaceholderData.value }]">
                <caption class="sr-only">{{ t("services.picker.results") }}</caption>
                <thead>
                  <tr>
                    <th scope="col" class="select-col">
                      <input
                        type="checkbox"
                        :checked="pageAllPicked"
                        :disabled="addable.length === 0 || (full && !pageAllPicked)"
                        :aria-label="t('services.members.selectPage')"
                        @change="pickPage(($event.target as HTMLInputElement).checked)"
                      />
                    </th>
                    <th scope="col">{{ t("services.col.name") }}</th>
                    <th scope="col">{{ t("services.members.col.class") }}</th>
                    <th scope="col">{{ t("services.col.criticality") }}</th>
                    <th scope="col">{{ t("services.col.active") }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="c in rows" :key="c.id" :class="{ selected: selected.has(c.id), disabled: isMember(c.id) }">
                    <td class="select-col">
                      <input
                        type="checkbox"
                        :checked="selected.has(c.id)"
                        :disabled="isMember(c.id) || (full && !selected.has(c.id))"
                        :aria-label="t('services.members.select', { name: c.label })"
                        :aria-describedby="isMember(c.id) ? `already-${uid}-${c.id}` : undefined"
                        @change="pick(c, ($event.target as HTMLInputElement).checked)"
                      />
                    </td>
                    <td>
                      <bdi>{{ c.label }}</bdi> <span class="mono muted impact-ident">{{ c.ident }}</span>
                      <span v-if="isMember(c.id)" :id="`already-${uid}-${c.id}`" class="badge">{{ t("services.picker.already") }}</span>
                    </td>
                    <td><bdi>{{ c.class.name }}</bdi></td>
                    <td><CriticalityBadge :value="c.criticality" show-unset /></td>
                    <td><CiStateBadge :ci="c" show-active /></td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div class="pagination">
              <span aria-live="polite">{{ t("services.picker.range", { from: from.toLocaleString(), to: to.toLocaleString(), total: total.toLocaleString() }) }}</span>
              <div class="actions">
                <button type="button" class="btn btn-sm" :disabled="offset === 0" @click="offset = Math.max(0, offset - PAGE)">‹ {{ t("services.picker.prev") }}</button>
                <button type="button" class="btn btn-sm" :disabled="offset + PAGE >= total" @click="offset += PAGE">{{ t("services.picker.next") }} ›</button>
              </div>
            </div>
          </template>

          <section class="picker-tray" :aria-labelledby="`tray-${uid}`">
            <h3 :id="`tray-${uid}`">{{ t("services.picker.selected", { n: selected.size }) }}</h3>
            <p v-if="full" class="hint" role="status">{{ t("services.picker.batchLimit", { max: limits.maxBatch }) }}</p>
            <p v-if="selected.size === 0" class="muted">{{ t("services.picker.selectedEmpty") }}</p>
            <ul v-else>
              <li v-for="p in selected.values()" :id="`pick-${uid}-${p.id}`" :key="p.id" tabindex="-1" :class="{ invalid: errors.has(p.id) }">
                <span>
                  <bdi>{{ p.label }}</bdi> <span class="muted">· <bdi>{{ p.className }}</bdi></span>
                  <span v-if="errors.get(p.id)" class="error">{{ errors.get(p.id)!.message }}</span>
                </span>
                <button type="button" class="btn btn-sm" :aria-label="t('services.picker.unselect', { name: p.label })" @click="unpick(p.id)">×</button>
              </li>
            </ul>
          </section>
        </div>
        <div class="footer">
          <button type="button" class="btn" :disabled="add.isPending.value" @click="requestClose">{{ t("services.picker.cancel") }}</button>
          <button type="submit" class="btn btn-primary" :disabled="selected.size === 0 || add.isPending.value">
            {{ add.isPending.value ? t("services.picker.adding") : t("services.picker.submit", { n: selected.size }) }}
          </button>
        </div>
      </form>
    </dialog>
    <ConfirmDialog
      :open="discarding"
      :title="t('services.picker.discardConfirm')"
      :confirm-label="t('services.picker.discard')"
      @confirm="close"
      @cancel="discarding = false"
    />
  </Teleport>
</template>
