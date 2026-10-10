<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate } from "vue-router";
import { useAllProfiles } from "../../../api/admin";
import { ApiError } from "../../../api/client";
import { useSaveGrants, useWorkflowDraft, useWorkflowGrants, useWorkflowVersion, type WorkflowDefinitionDetail, type WorkflowGrants } from "../../../api/workflows";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { t } from "../../../i18n";
import { useFlashStore } from "../../../stores/flash";
import { PSEUDO_GRANTS, grantRows, grantSets, grantsBody } from "../../../lib/workflowDraft";

/**
 * Who may run which transition: transitions (of the draft and the current version, by key) ×
 * permission profiles. The whole set is saved at once, guarded by the workflow's version. Group
 * grants come with approvals (Q5). Listing profiles needs `profiles.manage` or `users.manage`;
 * without it, the profiles already granted are shown and others are added by name. Saving goes through the
 * shared save bar, with a toast.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const flash = useFlashStore();
const wid = computed(() => props.workflow.id);
const grants = useWorkflowGrants(wid);
const draft = useWorkflowDraft(wid);
const current = useWorkflowVersion(wid, () => props.workflow.currentVersionNo);
const profiles = useAllProfiles();
const cannotListProfiles = computed(() => profiles.error.value instanceof ApiError && profiles.error.value.status === 403);

const rows = computed(() =>
  grantRows([draft.data.value?.transitions ?? [], current.data.value?.transitions ?? []], grants.data.value?.grants ?? []),
);
const loading = computed(() => grants.isLoading.value || draft.isLoading.value || current.isLoading.value);

/** Profiles added by name in this session (when profiles cannot be listed); the API resolves names. */
const added = ref<{ id: string; name: string }[]>([]);
const columns = computed(() => {
  const out = new Map<string, { id: string; name: string }>();
  for (const p of profiles.data.value?.data ?? []) out.set(p.id, { id: p.id, name: p.name });
  for (const g of grants.data.value?.grants ?? []) for (const p of g.profiles) if (!out.has(p.id)) out.set(p.id, p);
  for (const p of added.value) if (![...out.values()].some((c) => c.name.toLowerCase() === p.name.toLowerCase())) out.set(p.id, p);
  return [...out.values()].sort((a, b) => a.name.localeCompare(b.name));
});

// ---------- Local edits ----------

const serialize = (m: Map<string, Set<string>>) => JSON.stringify(grantsBody(m).sort((a, b) => a.transitionKey.localeCompare(b.transitionKey)));
const sets = ref(new Map<string, Set<string>>());
// The baseline starts as the empty matrix, so nothing counts as changed before the grants load and seed it.
const base = ref(serialize(sets.value));
const dirty = computed(() => serialize(sets.value) !== base.value);
function seed(g: WorkflowGrants) {
  sets.value = grantSets(g.grants);
  base.value = serialize(sets.value);
}
watch(
  () => grants.data.value,
  (g) => {
    if (g && !dirty.value) seed(g);
  },
  { immediate: true },
);

const has = (row: string, profile: string) => sets.value.get(row)?.has(profile) ?? false;
function toggle(row: string, profile: string, on: boolean) {
  const next = new Map(sets.value);
  const s = new Set(next.get(row) ?? []);
  if (on) s.add(profile);
  else s.delete(profile);
  next.set(row, s);
  sets.value = next;
}
function setRow(row: string, on: boolean) {
  const next = new Map(sets.value);
  next.set(row, new Set(on ? columns.value.map((c) => c.id) : []));
  sets.value = next;
}
const rowAll = (row: string) => columns.value.length > 0 && columns.value.every((c) => has(row, c.id));

const newProfile = ref("");
function addByName() {
  const name = newProfile.value.trim();
  if (!name) return;
  if (!columns.value.some((c) => c.name.toLowerCase() === name.toLowerCase())) added.value.push({ id: name, name });
  newProfile.value = "";
}

// ---------- Saving ----------

const save = useSaveGrants();
const error = ref<unknown>(null);
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");

async function submit() {
  const g = grants.data.value;
  if (!g) return;
  error.value = null;
  try {
    const next = await save.mutateAsync({ id: wid.value, body: { version: g.version, grants: grantsBody(sets.value) } });
    added.value = [];
    seed(next);
    flash.show(t("wfAdmin.grants.saved"));
  } catch (e) {
    error.value = e;
  }
}
async function reload() {
  error.value = null;
  const res = await grants.refetch();
  if (res.data) seed(res.data);
}
function reset() {
  if (grants.data.value) seed(grants.data.value);
  added.value = [];
  error.value = null;
}

// Unsaved grants: ask before another tab of this page (a ?tab= update) or another page drops them, and let the
// browser ask before a reload or closing the window.
const keepChanges = () => !dirty.value || window.confirm(t("wfAdmin.grants.leave"));
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate(keepChanges);
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!dirty.value) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

const ungranted = computed(() => rows.value.filter((r) => !PSEUDO_GRANTS.includes(r.key) && !r.orphan && !(sets.value.get(r.key)?.size ?? 0)).length);
</script>

<template>
  <section class="panel" aria-labelledby="wf-grants-title">
    <div class="panel-header">
      <h2 id="wf-grants-title">{{ t("wfAdmin.tab.grants") }}</h2>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfAdmin.grants.intro") }}</p>
      <div v-if="conflict" class="alert alert-warn" role="alert">
        <div>{{ t("wfAdmin.grants.conflict") }}</div>
        <div><button type="button" class="btn btn-sm" @click="reload">{{ t("wfAdmin.grants.reload") }}</button></div>
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('wfAdmin.grants.failed')" />
      <div v-if="cannotListProfiles" class="alert" role="note">{{ t("wfAdmin.grants.cannotList") }}</div>
      <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
      <p v-if="ungranted > 0" class="muted no-margin">{{ t("wfAdmin.grants.ungranted", { n: ungranted }) }}</p>
    </div>
    <LoadingState v-if="loading" :label="t('wfAdmin.grants.loading')" />
    <div v-else-if="grants.isError.value" class="panel-body"><ErrorAlert :error="grants.error.value" :on-retry="() => grants.refetch()" /></div>
    <EmptyState v-else-if="rows.length <= 1" icon="network" :title="t('wfAdmin.grants.empty.title')">{{ t("wfAdmin.grants.empty.body") }}</EmptyState>
    <template v-if="!loading && grants.data.value">
      <EmptyState v-if="columns.length === 0" icon="shield" :title="t('wfAdmin.grants.noProfiles')" />
      <div v-else class="table-wrap" role="region" tabindex="0" :aria-label="t('wfAdmin.grants.matrix')">
        <table class="data wf-grants" data-testid="wf-grants">
          <thead>
            <tr>
              <th scope="col">{{ t("wfAdmin.grants.transition") }}</th>
              <th v-for="c in columns" :key="c.id" scope="col" class="wf-grant-col" dir="auto">{{ c.name }}</th>
              <th scope="col"><span class="sr-only">{{ t("wfAdmin.grants.allProfiles") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.key">
              <th scope="row">
                <span dir="auto">{{ r.name }}</span> <span v-if="!PSEUDO_GRANTS.includes(r.key)" class="mono muted">{{ r.key }}</span>
                <span v-if="r.orphan" class="badge off spaced" :title="t('wfAdmin.grants.orphanTitle')">{{ t("wfAdmin.grants.orphan") }}</span>
              </th>
              <td v-for="c in columns" :key="c.id" class="wf-grant-cell">
                <input
                  type="checkbox"
                  :checked="has(r.key, c.id)"
                  :aria-label="t('wfAdmin.grants.cell', { profile: c.name, transition: r.name })"
                  @change="toggle(r.key, c.id, ($event.target as HTMLInputElement).checked)"
                />
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm" @click="setRow(r.key, !rowAll(r.key))">{{ rowAll(r.key) ? t("wfAdmin.grants.none") : t("wfAdmin.grants.all") }}</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <form v-if="cannotListProfiles" class="toolbar" @submit.prevent="addByName">
        <div class="field">
          <label for="wf-grant-profile">{{ t("wfAdmin.grants.addByName") }}</label>
          <input id="wf-grant-profile" v-model="newProfile" type="text" maxlength="200" autocomplete="off" />
        </div>
        <button type="submit" class="btn" :disabled="!newProfile.trim()">{{ t("wfAdmin.grants.addColumn") }}</button>
      </form>
    </template>
  </section>
  <SaveBar v-if="!loading && grants.data.value" :label="t('record.save.region')" :dirty="dirty">
    <button v-if="dirty" type="button" class="btn" :disabled="save.isPending.value" @click="reset">{{ t("record.save.discard") }}</button>
    <button type="button" class="btn btn-primary" :disabled="!dirty || save.isPending.value" @click="submit">
      {{ save.isPending.value ? t("common.saving") : t("wfAdmin.grants.save") }}
    </button>
  </SaveBar>
</template>
