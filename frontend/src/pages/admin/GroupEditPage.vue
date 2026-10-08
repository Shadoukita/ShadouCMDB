<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import type { User } from "../../api/admin";
import { ApiError } from "../../api/client";
import {
  MAX_GROUP_MEMBERS,
  useChangeGroupMembers,
  useCreateGroup,
  useDeleteGroup,
  useGroup,
  useGroupMembers,
  useUpdateGroup,
  type GroupMemberQuery,
  type GroupUpdateBody,
  type UserGroupDetail,
} from "../../api/groups";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SaveBar from "../../components/SaveBar.vue";
import { t } from "../../i18n";
import { useDebounced, useDocumentTitle, useUnsavedGuard } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { useFlashStore } from "../../stores/flash";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";
import UserPicker from "./UserPicker.vue";
import SortIcon from "../../components/SortIcon.vue";

/**
 * Administration › Groups › new / one group: name and description, the members (a replace of the
 * whole set behind each add and remove) and the delete, which first says how many business
 * services lose the group as owner. Title row, `⋯` menu and save bar as on the other admin edit
 * pages (design §2.7, audit A3); the members are saved one change at a time, outside the save bar.
 */
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const group = useGroup(id);
const create = useCreateGroup();
const update = useUpdateGroup();
const pending = computed(() => create.isPending.value || update.isPending.value);
useDocumentTitle(() => (isNew.value ? t("groups.new") : group.data.value?.name));

// ---------- Name and description ----------

interface Base {
  id: string;
  name: string;
  description: string;
  version: number;
}
const base = ref<Base | null>(null);
const form = ref({ name: "", description: "" });
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
/** Changed fields: against the stored group, or on a new group the ones filled in. */
const changes = computed(() => {
  const b = base.value ?? { name: "", description: "" };
  return (form.value.name !== b.name ? 1 : 0) + (form.value.description !== b.description ? 1 : 0);
});
const dirty = computed(() => changes.value > 0);
const guard = useUnsavedGuard(() => dirty.value, () => t("admin.unsaved.leave"));

function seed(g: UserGroupDetail) {
  base.value = { id: g.id, name: g.name, description: g.description ?? "", version: g.version };
  form.value = { name: base.value.name, description: base.value.description };
}

// Seed from the record, and again when it is refetched, unless that would overwrite unsaved edits.
watch(
  () => group.data.value,
  (g) => {
    if (g && (!dirty.value || base.value?.id !== g.id)) seed(g);
  },
  { immediate: true },
);
watch(id, () => {
  if (!id.value) {
    base.value = null;
    form.value = { name: "", description: "" };
  }
  error.value = null;
  local.value = {};
});

const FIELDS = ["name", "description"];
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field.split(".")[0])) : []));

async function submit() {
  error.value = null;
  const name = form.value.name.trim();
  const description = form.value.description.trim() || null;
  local.value = name ? {} : { name: t("common.required") };
  if (!name) {
    document.getElementById("group-name")?.focus();
    return;
  }
  try {
    if (isNew.value) {
      const created = await create.mutateAsync({ name, description });
      flash.show(t("groups.created", { name: created.name }));
      guard.allow();
      await router.push(`/admin/groups/${created.id}`);
      return;
    }
    const b = base.value!;
    const body: GroupUpdateBody = { version: b.version };
    if (name !== b.name) body.name = name;
    if (description !== (b.description || null)) body.description = description;
    if (body.name === undefined && body.description === undefined) {
      flash.show(t("common.nothingChanged"));
      return;
    }
    const next = await update.mutateAsync({ id: b.id, body });
    seed({ ...group.data.value!, ...next });
    flash.show(t("groups.saved", { name: next.name }));
  } catch (e) {
    error.value = e;
  }
}

/** Back to the stored values. */
function discard() {
  form.value = base.value ? { name: base.value.name, description: base.value.description } : { name: "", description: "" };
  error.value = null;
  local.value = {};
}

/** After a 409: drop the edits and show what is stored now. */
async function reloadAfterConflict() {
  const res = await group.refetch();
  if (res.data) seed(res.data);
  error.value = null;
}

// ---------- Members ----------

const lq = useListQuery({ sort: "username" });
const memberQuery = computed<GroupMemberQuery>(() => ({
  q: lq.get("q") || undefined,
  sort: lq.sort.value as GroupMemberQuery["sort"],
  limit: lq.limit.value,
  offset: lq.offset.value,
}));
const members = useGroupMembers(id, memberQuery);
const memberRows = computed(() => members.data.value?.data ?? []);
const memberTotal = computed(() => members.data.value?.page.total ?? 0);
const memberCount = computed(() => group.data.value?.memberCount ?? 0);
const atLimit = computed(() => memberCount.value >= MAX_GROUP_MEMBERS);
const MEMBER_COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "username", label: t("groups.members.col.username"), sort: "username" },
  { key: "displayName", label: t("groups.members.col.displayName"), sort: "displayName" },
  { key: "status", label: t("groups.members.col.status") },
  { key: "added", label: t("groups.members.col.added"), sort: "addedAt" },
];

const mText = ref(lq.get("q"));
const debouncedM = useDebounced(mText, 300);
watch(debouncedM, (v) => v !== lq.get("q") && lq.update({ q: v || undefined }));
watch(
  () => lq.get("q"),
  (v) => (mText.value = v),
);

const changeMembers = useChangeGroupMembers();
const memberStatus = ref<string | null>(null);
const memberError = ref<unknown>(null);
const memberConflict = ref(false);

async function changeMember(change: { add?: string[]; remove?: string[] }, done: string) {
  const g = group.data.value;
  if (!g) return;
  memberStatus.value = null;
  memberError.value = null;
  memberConflict.value = false;
  try {
    const next = await changeMembers.mutateAsync({ id: g.id, version: g.version, ...change });
    // Our own change moved the version on: unsaved name edits based on the old one stay valid.
    if (base.value && base.value.version === g.version) base.value.version = next.version;
    memberStatus.value = done;
  } catch (e) {
    if (e instanceof ApiError && e.code === "VERSION_CONFLICT") {
      memberConflict.value = true;
      await group.refetch();
    } else {
      memberError.value = e;
    }
  }
}

const addMember = (u: User) => changeMember({ add: [u.id] }, t("groups.members.added", { name: u.username }));
const removeMember = (m: { id: string; username: string }) =>
  changeMember({ remove: [m.id] }, t("groups.members.removed", { name: m.username }));

// ---------- Delete ----------

const del = useDeleteGroup();
const deleting = ref(false);
const deleteChecked = ref(false);

/** Re-read the group so the dialog names the current number of owned services. */
async function openDelete() {
  del.reset();
  deleteChecked.value = false;
  deleting.value = true;
  await group.refetch();
  deleteChecked.value = true;
}

function confirmDelete() {
  const g = group.data.value;
  if (!g) return;
  del.mutate(g.id, {
    onSuccess: (res) => {
      // A withheld count (null) is not repeated: the dialog already said so.
      const n = res?.affectedServices;
      const services = typeof n === "number" ? t("groups.deleted.services", { n }) : "";
      flash.show(`${t("groups.deleted", { name: g.name })} ${services}`.trim());
      guard.allow();
      router.replace("/admin/groups");
    },
  });
}

const moreActions = computed<RowMenuItem[]>(() => [{ label: t("groups.delete.button"), danger: true, action: openDelete }]);

const crumbs = computed(() => adminCrumbs("groups", { label: isNew.value ? t("groups.new") : (group.data.value?.name ?? "…") }));
const notFound = computed(() => {
  const e = group.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs v-if="!isNew && (group.isLoading.value || (group.isError.value && !group.data.value))" :items="crumbs" />
  <LoadingState v-if="!isNew && group.isLoading.value" :label="t('groups.loadingOne')" />
  <template v-else-if="!isNew && group.isError.value && !group.data.value">
    <EmptyState v-if="notFound" :title="t('groups.notFound.title')">
      {{ t("groups.notFound.body", { id: id }) }}
      <template #actions><RouterLink class="btn" to="/admin/groups">{{ t("groups.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="group.error.value" :on-retry="() => group.refetch()" />
  </template>
  <template v-else>
    <div class="record-head record-head-plain">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="users" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ isNew ? t("groups.new") : group.data.value?.name }}</h1>
            </div>
            <p v-if="group.data.value && !isNew" class="record-meta" data-testid="record-meta">
              <span class="badge">{{ t("groups.members.count", { n: memberCount }) }}</span>
              <span class="record-meta-line">
                <time :datetime="group.data.value.updatedAt" :title="formatDateTime(group.data.value.updatedAt)">
                  {{ t("record.meta.updated", { when: formatRelative(group.data.value.updatedAt) }) }}
                </time>
              </span>
            </p>
          </div>
        </div>
        <div v-if="group.data.value && !isNew" class="actions">
          <RowMenu :label="t('record.actions.more')" :items="moreActions" large />
        </div>
      </div>
    </div>

    <div v-if="conflict" class="alert alert-warn" role="alert" data-testid="group-conflict">
      <div>{{ t("groups.conflict") }}</div>
      <div><button type="button" class="btn btn-sm" @click="reloadAfterConflict">{{ t("groups.conflictReload") }}</button></div>
    </div>
    <FormErrorBanner v-else-if="error" :error="error" :unplaced="unplaced" />
    <div class="grid-2">
      <form id="group-form" class="panel" aria-labelledby="group-form-title" novalidate @submit.prevent="submit">
        <div class="panel-header"><h2 id="group-form-title">{{ t("groups.form.title") }}</h2></div>
        <div class="panel-body">
          <div class="form-grid">
            <FormField id="group-name" :label="t('groups.field.name')" required :error="fieldErrors.name" :hint="t('groups.field.nameHint')">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.name" v-autofocus="isNew" type="text" maxlength="200" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <FormField id="group-description" :label="t('groups.field.description')" :error="fieldErrors.description" :hint="t('groups.field.descriptionHint')">
              <template #default="{ id: fid, invalid, describedBy }">
                <textarea :id="fid" v-model="form.description" rows="3" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
          </div>
        </div>
      </form>

      <section v-if="group.data.value && !isNew" class="panel" aria-labelledby="group-facts-title">
        <div class="panel-header"><h2 id="group-facts-title">{{ t("groups.facts.title") }}</h2></div>
        <div class="panel-body">
          <dl class="props">
            <dt>{{ t("groups.members") }}</dt>
            <dd>{{ memberCount.toLocaleString() }}</dd>
            <dt>{{ t("groups.facts.ownedServices") }}</dt>
            <dd v-if="group.data.value.ownedServiceCount === null" class="muted">{{ t("groups.facts.withheld") }}</dd>
            <dd v-else>{{ group.data.value.ownedServiceCount.toLocaleString() }}</dd>
            <dt>{{ t("common.created") }}</dt>
            <dd>{{ formatDateTime(group.data.value.createdAt) }}</dd>
            <dt>{{ t("common.updated") }}</dt>
            <dd>{{ formatDateTime(group.data.value.updatedAt) }}</dd>
          </dl>
        </div>
      </section>
    </div>

    <SaveBar :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
      <RouterLink class="btn" to="/admin/groups">{{ t("common.cancel") }}</RouterLink>
      <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="group-form" class="btn btn-primary" :disabled="pending">
        {{ pending ? t("common.saving") : isNew ? t("groups.create") : t("common.saveChanges") }}
      </button>
    </SaveBar>

    <section v-if="group.data.value && !isNew" class="panel" aria-labelledby="group-members-title">
      <div class="panel-header">
        <h2 id="group-members-title">{{ t("groups.members") }}</h2>
        <span v-if="members.isFetching.value && !members.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="toolbar">
        <UserPicker
          id="group-add-member"
          :label="t('groups.members.add')"
          :placeholder="t('groups.members.addPlaceholder')"
          :disabled="atLimit || changeMembers.isPending.value"
          @select="addMember"
        />
        <div class="field search">
          <label for="group-member-q">{{ t("groups.members.filter") }}</label>
          <input id="group-member-q" v-model="mText" type="search" />
        </div>
      </div>
      <div class="panel-body stack">
        <p v-if="atLimit" class="muted no-margin">{{ t("groups.members.limit", { max: MAX_GROUP_MEMBERS }) }}</p>
        <div class="sr-only" aria-live="polite">{{ memberStatus }}</div>
        <div v-if="memberStatus" class="alert" data-testid="group-member-status">{{ memberStatus }}</div>
        <div v-if="memberConflict" class="alert alert-warn" role="alert">{{ t("groups.members.conflict") }}</div>
        <ErrorAlert v-if="memberError" :error="memberError" :title="t('groups.members.failed')" />
        <ErrorAlert v-if="members.isError.value" :error="members.error.value" :on-retry="() => members.refetch()" />
        <p class="muted no-margin">{{ t("groups.members.disabledNote") }}</p>
      </div>
      <LoadingState v-if="members.isLoading.value" :label="t('groups.loadingOne')" />
      <EmptyState v-else-if="members.data.value && memberTotal === 0" :title="lq.get('q') ? t('groups.members.noMatch') : t('groups.members.empty')" />
      <EmptyState v-else-if="members.data.value && memberTotal > 0 && memberRows.length === 0" :title="t('common.pastEnd')">
        <template #actions><button type="button" class="btn" @click="lq.update({})">{{ t("common.firstPage") }}</button></template>
      </EmptyState>
      <template v-if="memberRows.length > 0">
        <div class="table-wrap">
          <table :class="['data', 'list-table', { loading: members.isPlaceholderData.value }]">
            <thead>
              <tr>
                <th v-for="c in MEMBER_COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                  <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ c.label }} <SortIcon :dir="lq.ariaSort(c.sort)" /></button>
                  <template v-else>{{ c.label }}</template>
                </th>
                <th scope="col"><span class="sr-only">{{ t("groups.members.remove") }}</span></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="m in memberRows" :key="m.id" :class="{ disabled: !m.isActive }">
                <td><RouterLink class="list-name" :to="`/admin/users/${m.id}`">{{ m.username }}</RouterLink></td>
                <td>{{ m.displayName }}</td>
                <td>
                  <span v-if="m.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
                  <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("common.disabled") }}</span>
                </td>
                <td :title="m.addedAt">{{ formatRelative(m.addedAt) }}</td>
                <td class="row-actions">
                  <button
                    type="button"
                    class="btn btn-sm"
                    :disabled="changeMembers.isPending.value"
                    :aria-label="t('groups.members.removeLabel', { name: m.username, group: group.data.value.name })"
                    @click="removeMember(m)"
                  >
                    {{ t("groups.members.remove") }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="table-footer">
          <PaginationBar numbered :total="memberTotal" :limit="lq.limit.value" :offset="lq.offset.value" @change="lq.onPage" />
        </div>
      </template>
    </section>

    <ConfirmDialog
      v-if="group.data.value && !isNew"
      :open="deleting"
      :title="t('groups.delete.title', { name: group.data.value.name })"
      :confirm-label="t('groups.delete.button')"
      :busy="del.isPending.value || !deleteChecked"
      @cancel="deleting = false"
      @confirm="confirmDelete"
    >
      <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('groups.delete.failed')" />
      <p v-if="!deleteChecked" class="muted">{{ t("groups.delete.checking") }}</p>
      <template v-else>
        <p data-testid="group-delete-services">
          <template v-if="group.data.value.ownedServiceCount === null">{{ t("groups.delete.bodyWithheld") }}</template>
          <template v-else-if="group.data.value.ownedServiceCount > 0">{{ t("groups.delete.body", { n: group.data.value.ownedServiceCount }) }}</template>
          <template v-else>{{ t("groups.delete.none") }}</template>
        </p>
        <p>{{ t("groups.delete.members", { n: memberCount }) }}</p>
      </template>
    </ConfirmDialog>
  </template>
</template>
