<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useCiClasses } from "../../api/queries";
import { downloadMembersCsv, useRemoveMembers, useServiceMembers, type ServiceMember } from "../../api/services";
import CiLink from "../../components/CiLink.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu from "../../components/RowMenu.vue";
import { t } from "../../i18n";
import { useDebounced } from "../../lib/composables";
import { formatDateTime } from "../../lib/format";
import {
  hasMemberFilters,
  MEMBERS_PAGE_SIZE,
  memberFilters,
  memberListQuery,
  membersUrlQuery,
  parseMembersQuery,
  type MemberKind,
  type MembersState,
} from "../../lib/serviceMembers";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import MemberPickerDialog from "./MemberPickerDialog.vue";

/**
 * A business service's Members tab (spec SHAA-927 §5.3): search, Class and Kind filters, sort and page in the URL
 * (`mq`, `mclass`, `mkind`, `msort`, `mpage`, next to the page's own `tab`), server paging, selection with
 * "Remove selected", a row menu, "Add members" (the picker) and the CSV export. Only members the caller may view
 * are listed; when their profile hides classes a static note says so, whatever the data.
 */
const props = defineProps<{
  service: { id: string; ident: string; name: string };
  canEdit: boolean;
  limits: { maxBatch: number; maxMembers: number; maxNesting: number } | undefined;
  self: TrailStep;
  trail: TrailStep[];
}>();

const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const classes = useCiClasses();

const state = computed(() => parseMembersQuery(route.query));
function setState(patch: Partial<MembersState>, replace = false) {
  // A new filter or sort starts again on the first page.
  const next = { ...state.value, page: 1, ...patch };
  const to = { path: route.path, query: membersUrlQuery(route.query, next) };
  return replace ? router.replace(to) : router.push(to);
}

// Search box: typed locally, debounced into the URL; back/forward puts the URL's text back.
const qText = ref(state.value.q);
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => {
  if (v !== state.value.q) void setState({ q: v });
});
watch(
  () => state.value.q,
  (v) => {
    if (v !== debouncedQ.value) qText.value = v;
  },
);

/** The Class filter offers only classes the caller may view: another one is refused by the API anyway. */
const classChoices = computed(() =>
  (classes.data.value ?? []).filter((c) => !c.isAbstract && session.canOnClass(c.id, "view")).sort((a, b) => a.name.localeCompare(b.name)),
);
const KINDS: { value: MemberKind; key: "services.members.kind.all" | "services.members.kind.ci" | "services.members.kind.service" }[] = [
  { value: "", key: "services.members.kind.all" },
  { value: "ci", key: "services.members.kind.ci" },
  { value: "service", key: "services.members.kind.service" },
];

const members = useServiceMembers(() => props.service.id, () => memberListQuery(state.value));
const rows = computed(() => members.data.value?.data ?? []);
const total = computed(() => members.data.value?.page.total ?? 0);
const restricted = computed(() => members.data.value?.visibility === "restricted");
const filtered = computed(() => hasMemberFilters(state.value));
const apiError = computed(() => (members.error.value instanceof ApiError ? members.error.value : null));
const busy = computed(() => !!apiError.value && ["RATE_LIMITED", "SERVER_BUSY"].includes(apiError.value.code));
// A hand-edited link with a class the caller may not view (or that does not exist): drop the filter.
watch(apiError, (e) => {
  if (e?.code === "VALIDATION_ERROR" && e.details.some((d) => d.field === "classId") && state.value.classIds.length > 0) {
    void setState({ classIds: [] }, true);
  }
});

// A bookmarked page past the end (members were removed since): show the last page instead.
watch(
  () => members.data.value,
  (d) => {
    if (d && d.data.length === 0 && d.page.total > 0 && state.value.page > 1) {
      void setState({ ...state.value, page: Math.ceil(d.page.total / MEMBERS_PAGE_SIZE) }, true);
    }
  },
);

function clearFilters() {
  qText.value = "";
  void setState({ q: "", classIds: [], kind: "" });
}

// ---------- Sorting ----------
const SORTABLE = [
  { key: "name", label: () => t("services.col.name") },
  { key: "class", label: () => t("services.members.col.class") },
  { key: "criticality", label: () => t("services.col.criticality") },
] as const;
const field = computed(() => state.value.sort.replace(/^-/, ""));
const ariaSort = (key: string) => (field.value !== key ? "none" : state.value.sort.startsWith("-") ? "descending" : "ascending");
const indicator = (key: string) => (field.value !== key ? "" : state.value.sort.startsWith("-") ? "▼" : "▲");
const toggleSort = (key: string) => setState({ sort: (state.value.sort === key ? `-${key}` : key) as MembersState["sort"] });

// ---------- Selection (this page) ----------
const selected = ref(new Set<string>());
watch(rows, (now) => {
  // Keep only what is still on screen: a refetch can drop a row.
  const ids = new Set(now.map((m) => m.ci.id));
  selected.value = new Set([...selected.value].filter((id) => ids.has(id)));
});
const allSelected = computed(() => rows.value.length > 0 && rows.value.every((m) => selected.value.has(m.ci.id)));
const someSelected = computed(() => selected.value.size > 0 && !allSelected.value);
function toggle(id: string, on: boolean) {
  const next = new Set(selected.value);
  if (on) next.add(id);
  else next.delete(id);
  selected.value = next;
}
function toggleAll(on: boolean) {
  selected.value = on ? new Set(rows.value.map((m) => m.ci.id)) : new Set();
}

// ---------- Messages ----------
/** The polite live region: what was added or removed. */
const announcement = ref("");
function announce(text: string) {
  announcement.value = "";
  void nextTick(() => (announcement.value = text));
}

// ---------- Remove ----------
const remove = useRemoveMembers(() => props.service.id);
const removing = ref<string[] | null>(null);
const removeError = ref<unknown>(null);
function askRemove(ids: string[]) {
  removeError.value = null;
  removing.value = ids;
}
async function confirmRemove() {
  const ids = removing.value ?? [];
  try {
    await remove.mutateAsync(ids);
    removing.value = null;
    selected.value = new Set();
    announce(t("services.members.removed", { n: ids.length }));
    // "Remove selected" is disabled now that nothing is selected: keep focus in the toolbar.
    void nextTick(() => addButton.value?.focus());
  } catch (e) {
    removing.value = null;
    removeError.value = e;
  }
}
function rowMenu(m: ServiceMember) {
  return [
    { label: t("services.members.open"), to: `/cis/${m.ci.id}` },
    { label: t("services.members.impact"), to: `/cis/${m.ci.id}/impact` },
    ...(props.canEdit ? [{ label: t("services.members.remove"), action: () => askRemove([m.ci.id]), danger: true }] : []),
  ];
}

// ---------- Add ----------
const picking = ref(false);
const addButton = ref<HTMLButtonElement>();
function closePicker() {
  picking.value = false;
  void nextTick(() => addButton.value?.focus());
}
function onAdded(added: number, already: number) {
  closePicker();
  announce([added > 0 || already === 0 ? t("services.picker.added", { n: added }) : "", already > 0 ? t("services.picker.alreadyMembers", { n: already }) : ""].filter(Boolean).join(" "));
}

// ---------- Export ----------
const exporting = ref(false);
const exportError = ref<unknown>(null);
async function exportCsv() {
  exporting.value = true;
  exportError.value = null;
  try {
    const now = new Date();
    const pad = (n: number) => String(n).padStart(2, "0");
    const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
    await downloadMembersCsv(props.service.id, memberFilters(state.value), `service-members-${props.service.ident}-${stamp}.csv`);
  } catch (e) {
    exportError.value = e;
  } finally {
    exporting.value = false;
  }
}

const kindLabel = (m: ServiceMember) => (m.isService ? t("services.badge") : t("services.members.kindValue.ci"));
</script>

<template>
  <section class="panel service-members" aria-labelledby="members-title">
    <h2 id="members-title" class="sr-only">{{ t("services.members.caption", { service: service.name }) }}</h2>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="m-q">{{ t("services.members.search") }}</label>
        <input id="m-q" v-model="qText" type="search" :placeholder="t('services.members.searchPlaceholder')" maxlength="200" />
      </div>
      <div class="field">
        <label for="m-class">{{ t("services.members.class") }}</label>
        <select id="m-class" :value="state.classIds[0] ?? ''" @change="setState({ classIds: ($event.target as HTMLSelectElement).value ? [($event.target as HTMLSelectElement).value] : [] })">
          <option value="">{{ t("services.members.allClasses") }}</option>
          <option v-for="c in classChoices" :key="c.id" :value="c.id">{{ c.name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="m-kind">{{ t("services.members.kind") }}</label>
        <select id="m-kind" :value="state.kind" @change="setState({ kind: ($event.target as HTMLSelectElement).value as MemberKind })">
          <option v-for="k in KINDS" :key="k.value" :value="k.value">{{ t(k.key) }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">{{ t("services.filter.clear") }}</button>
      <div class="toolbar-end service-members-actions">
        <span v-if="members.isFetching.value && members.data.value" class="spinner" aria-hidden="true" />
        <button type="button" class="btn" :disabled="exporting" @click="exportCsv">
          {{ exporting ? t("services.members.exporting") : t("services.export") }}
        </button>
        <template v-if="canEdit">
          <button type="button" class="btn" :disabled="selected.size === 0" @click="askRemove([...selected])">
            {{ t("services.members.removeSelected") }}<template v-if="selected.size > 0"> ({{ selected.size }})</template>
          </button>
          <button ref="addButton" type="button" class="btn btn-primary" @click="picking = true">{{ t("services.members.add") }}</button>
        </template>
      </div>
    </form>

    <div class="panel-body service-members-notes">
      <div aria-live="polite" role="status" class="service-members-live">
        <div v-if="announcement" class="alert">{{ announcement }}</div>
      </div>
      <p v-if="restricted" class="muted" role="note">{{ t("services.members.restricted") }}</p>
      <ErrorAlert v-if="exportError" :error="exportError" :title="t('services.members.exportFailed')" />
      <ErrorAlert v-if="removeError" :error="removeError" :title="t('services.members.removeFailed')" />
    </div>

    <div v-if="busy" class="panel-body">
      <div class="alert alert-warn" role="alert">
        <div>{{ t("common.busy") }}</div>
        <div class="meta">
          <template v-if="apiError?.requestId">Request id <code>{{ apiError.requestId }}</code>&#32;</template>
          <button type="button" class="btn btn-sm" @click="members.refetch()">{{ t("common.retry") }}</button>
        </div>
      </div>
    </div>
    <div v-else-if="members.isError.value" class="panel-body">
      <ErrorAlert :error="members.error.value" :title="t('services.members.loadFailed')" :on-retry="() => members.refetch()" />
    </div>

    <div v-if="members.isPending.value" class="panel-body" aria-busy="true">
      <p class="muted" role="status"><span class="spinner" aria-hidden="true" /> {{ t("services.members.loading") }}</p>
      <div class="skeleton-table" aria-hidden="true"><div v-for="n in 5" :key="n" class="skeleton-row" /></div>
    </div>
    <EmptyState v-else-if="members.data.value && total === 0 && !filtered" :title="t('services.members.empty')">
      <template v-if="canEdit" #actions>
        <button type="button" class="btn btn-primary" @click="picking = true">{{ t("services.members.add") }}</button>
      </template>
    </EmptyState>
    <EmptyState v-else-if="members.data.value && total === 0" :title="t('services.members.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("services.filter.clear") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap" role="region" tabindex="0" :aria-label="t('services.members.caption', { service: service.name })">
        <table :class="['data', { loading: members.isPlaceholderData.value }]">
          <caption class="sr-only">{{ t("services.members.caption", { service: service.name }) }}</caption>
          <thead>
            <tr>
              <th v-if="canEdit" scope="col" class="select-col">
                <input
                  type="checkbox"
                  :checked="allSelected"
                  :indeterminate="someSelected"
                  :aria-label="t('services.members.selectPage')"
                  @change="toggleAll(($event.target as HTMLInputElement).checked)"
                />
              </th>
              <th v-for="c in SORTABLE" :key="c.key" scope="col" :aria-sort="ariaSort(c.key)">
                <button type="button" class="sort" @click="toggleSort(c.key)">{{ c.label() }} {{ indicator(c.key) }}</button>
              </th>
              <th scope="col">{{ t("services.members.col.kind") }}</th>
              <th scope="col">{{ t("services.col.active") }}</th>
              <th scope="col" :aria-sort="ariaSort('addedAt')">
                <button type="button" class="sort" @click="toggleSort('addedAt')">{{ t("services.members.col.added") }} {{ indicator("addedAt") }}</button>
              </th>
              <th scope="col"><span class="sr-only">{{ t("services.members.col.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="m in rows" :key="m.membershipId" :class="{ selected: selected.has(m.ci.id) }">
              <td v-if="canEdit" class="select-col">
                <input
                  type="checkbox"
                  :checked="selected.has(m.ci.id)"
                  :aria-label="t('services.members.select', { name: m.ci.name })"
                  @change="toggle(m.ci.id, ($event.target as HTMLInputElement).checked)"
                />
              </td>
              <td>
                <CiLink :id="m.ci.id" :from="self" :trail="trail">{{ m.ci.name }}</CiLink>
                <span class="mono muted impact-ident">{{ m.ci.ident }}</span>
              </td>
              <td><bdi>{{ m.ci.className }}</bdi></td>
              <td><CriticalityBadge :value="m.ci.criticality" show-unset /></td>
              <td>{{ kindLabel(m) }}</td>
              <td><CiStateBadge :ci="m.ci" show-active /></td>
              <td>{{ formatDateTime(m.addedAt) }}</td>
              <td class="row-actions">
                <RowMenu :label="t('services.members.actionsFor', { name: m.ci.name })" :items="rowMenu(m)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar
        v-if="total > MEMBERS_PAGE_SIZE || state.page > 1"
        :total="total"
        :limit="MEMBERS_PAGE_SIZE"
        :offset="(state.page - 1) * MEMBERS_PAGE_SIZE"
        @change="(p) => setState({ ...state, page: Math.floor(p.offset / MEMBERS_PAGE_SIZE) + 1 })"
      />
    </template>

    <ConfirmDialog
      :open="removing !== null"
      :title="t('services.members.removeTitle')"
      :confirm-label="t('services.members.removeAction')"
      :busy="remove.isPending.value"
      @confirm="confirmRemove"
      @cancel="removing = null"
    >
      {{ t("services.members.removeConfirm", { n: removing?.length ?? 0, service: service.name }) }}
    </ConfirmDialog>
    <MemberPickerDialog
      v-if="picking && limits"
      :service="service"
      :limits="limits"
      @close="closePicker"
      @added="onAdded"
    />
  </section>
</template>
