<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useAllProfiles } from "../../../api/admin";
import { ApiError } from "../../../api/client";
import { useSaveGrants, useWorkflowDraft, useWorkflowGrants, useWorkflowVersion, type WorkflowDefinitionDetail, type WorkflowGrants } from "../../../api/workflows";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { CANCEL_GRANT, grantRows, grantSets, grantsBody } from "../../../lib/workflowDraft";

/**
 * Who may run which transition: transitions (of the draft and the current version, by key) ×
 * permission profiles. The whole set is saved at once, guarded by the workflow's version. Group
 * grants come with approvals (Q5). Listing profiles needs `profiles.manage` or `users.manage`;
 * without it, the profiles already granted are shown and others are added by name.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
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
  saved.value = null;
}
function setRow(row: string, on: boolean) {
  const next = new Map(sets.value);
  next.set(row, new Set(on ? columns.value.map((c) => c.id) : []));
  sets.value = next;
  saved.value = null;
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
const saved = ref<string | null>(null);
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");

async function submit() {
  const g = grants.data.value;
  if (!g) return;
  error.value = null;
  saved.value = null;
  try {
    const next = await save.mutateAsync({ id: wid.value, body: { version: g.version, grants: grantsBody(sets.value) } });
    added.value = [];
    seed(next);
    saved.value = "Grants saved.";
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
const ungranted = computed(() => rows.value.filter((r) => r.key !== CANCEL_GRANT && !r.orphan && !(sets.value.get(r.key)?.size ?? 0)).length);
</script>

<template>
  <section class="panel" aria-labelledby="wf-grants-title">
    <div class="panel-header">
      <h2 id="wf-grants-title">Grants</h2>
      <span v-if="dirty" class="badge warn">Unsaved changes</span>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">
        Tick the permission profiles whose holders may run each transition. Running one also needs the edit right on the CI's type.
        Holders of the Administrator profile may run every transition. An API token runs a transition only if its own profile is
        granted too. Grants apply to every version: they are given by transition key.
      </p>
      <div v-if="conflict" class="alert alert-warn" role="alert">
        <div>Someone changed this workflow (its grants, settings or versions) since you opened it. Your grants were not saved.</div>
        <div><button type="button" class="btn btn-sm" @click="reload">Load the current grants</button></div>
      </div>
      <ErrorAlert v-else-if="error" :error="error" title="The grants were not saved" />
      <div v-if="saved" class="alert" role="status">{{ saved }}</div>
      <div v-if="cannotListProfiles" class="alert" role="note">
        You may not list permission profiles (that needs Manage permission profiles or Manage users). The profiles already granted are
        shown; add others by their name.
      </div>
      <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
      <p v-if="ungranted > 0" class="muted no-margin">
        {{ ungranted }} {{ ungranted === 1 ? "transition has" : "transitions have" }} no profile: only administrators can run
        {{ ungranted === 1 ? "it" : "them" }}.
      </p>
    </div>
    <LoadingState v-if="loading" label="Loading grants…" />
    <div v-else-if="grants.isError.value" class="panel-body"><ErrorAlert :error="grants.error.value" :on-retry="() => grants.refetch()" /></div>
    <EmptyState v-else-if="rows.length <= 1" title="No transitions yet">
      Add transitions in the Designer tab; then decide here who may run them. Cancelling an instance can be granted already.
    </EmptyState>
    <template v-if="!loading && grants.data.value">
      <div v-if="columns.length === 0" class="panel-body muted">No permission profiles to grant.</div>
      <div v-else class="table-wrap">
        <table class="data wf-grants" data-testid="wf-grants">
          <thead>
            <tr>
              <th scope="col">Transition</th>
              <th v-for="c in columns" :key="c.id" scope="col" class="wf-grant-col">{{ c.name }}</th>
              <th scope="col"><span class="sr-only">All profiles</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.key">
              <th scope="row">
                {{ r.name }} <span v-if="r.key !== CANCEL_GRANT" class="mono muted">{{ r.key }}</span>
                <span v-if="r.orphan" class="badge off spaced" title="No longer in the draft or the current version">Older version</span>
              </th>
              <td v-for="c in columns" :key="c.id" class="wf-grant-cell">
                <input type="checkbox" :checked="has(r.key, c.id)" :aria-label="`${c.name} may run ${r.name}`" @change="toggle(r.key, c.id, ($event.target as HTMLInputElement).checked)" />
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm" @click="setRow(r.key, !rowAll(r.key))">{{ rowAll(r.key) ? "None" : "All" }}</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <form v-if="cannotListProfiles" class="toolbar" @submit.prevent="addByName">
        <div class="field">
          <label for="wf-grant-profile">Add a profile by name</label>
          <input id="wf-grant-profile" v-model="newProfile" type="text" maxlength="200" autocomplete="off" />
        </div>
        <button type="submit" class="btn" :disabled="!newProfile.trim()">Add column</button>
      </form>
      <div class="form-footer">
        <button type="button" class="btn btn-primary" :disabled="!dirty || save.isPending.value" @click="submit">
          {{ save.isPending.value ? "Saving…" : "Save grants" }}
        </button>
        <button type="button" class="btn" :disabled="!dirty || save.isPending.value" @click="reset">Undo changes</button>
      </div>
    </template>
  </section>
</template>
