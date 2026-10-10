<script setup lang="ts">
import { t } from "../../i18n";
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import {
  useCreateProfile,
  useProfile,
  useUpdateProfile,
  type ClassPermission,
  type GlobalPermission,
  type PermissionProfile,
  type ProfileUpdateBody,
} from "../../api/admin";
import { ApiError } from "../../api/client";
import { useCiClasses } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SaveBar from "../../components/SaveBar.vue";
import { changedFields } from "../../lib/changes";
import { useDocumentTitle, useUnsavedGuard } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { formatDate, formatDateTime, formatRelative } from "../../lib/format";
import { GLOBAL_PERMISSIONS, type ClassRight } from "../../lib/permissions";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";
import CloneProfileDialog from "./CloneProfileDialog.vue";
import DeleteProfileDialog from "./DeleteProfileDialog.vue";

/**
 * Create or edit a permission profile: name, global permissions and the class
 * matrix (view/create/edit/delete per class, plus an "all classes" row that also
 * covers classes created later). The matrix rows come from the API's class list; its columns are grouped
 * into Read (view) and Change (create, edit, delete), with the class column and the header kept in view
 * while it scrolls (audit A7). Title row, `⋯` menu and save bar as on the other admin edit pages (A3).
 */
type Rights = Record<ClassRight, boolean>;
const WILDCARD = "*";
const NONE: Rights = { view: false, create: false, edit: false, delete: false };

const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const profile = useProfile(id);
const classes = useCiClasses();
const create = useCreateProfile();
const update = useUpdateProfile();
const pending = computed(() => create.isPending.value || update.isPending.value);
useDocumentTitle(() => (isNew.value ? t("admin.profile.new") : profile.data.value?.name));

const builtin = computed(() => !!profile.data.value?.isBuiltin);
const canManage = computed(() => session.can("profiles.manage"));
/** Name and permissions. The built-in Administrator profile only takes `requireMfa`. */
const readOnly = computed(() => builtin.value || !canManage.value);

const name = ref("");
const description = ref("");
const globals = ref<GlobalPermission[]>([]);
const requireMfa = ref(false);
/** Explicit grants keyed by class id (WILDCARD for "all classes"). */
const grants = ref<Record<string, Rights>>({});
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const cloning = ref<PermissionProfile | null>(null);

function seed(p: PermissionProfile | undefined) {
  name.value = p?.name ?? "";
  description.value = p?.description ?? "";
  globals.value = p ? [...p.globalPermissions] : [];
  requireMfa.value = p?.requireMfa ?? false;
  const g: Record<string, Rights> = {};
  for (const c of p?.classPermissions ?? []) g[c.classId ?? WILDCARD] = { view: c.view, create: c.create, edit: c.edit, delete: c.delete };
  grants.value = g;
  initial = p ? formBody() : {};
}
/** The form as loaded, to send only changed fields on save. */
let initial: ProfileUpdateBody = {};
watch(() => profile.data.value, seed, { immediate: true });
watch(id, () => {
  if (!id.value) seed(undefined);
  error.value = null;
  local.value = {};
});

/** Matrix rows: every concrete class, plus any other class this profile already grants on. */
const rows = computed(() =>
  (classes.data.value ?? [])
    .filter((c) => !c.isAbstract || grants.value[c.id])
    .map((c) => ({ id: c.id, name: c.name, isActive: c.isActive, isAbstract: c.isAbstract })),
);
const unknownGrants = computed(() => {
  if (!classes.data.value) return [];
  const known = new Set(classes.data.value.map((c) => c.id));
  return Object.keys(grants.value).filter((k) => k !== WILDCARD && !known.has(k));
});

const rightsOf = (key: string): Rights => grants.value[key] ?? NONE;
const wildcard = computed(() => rightsOf(WILDCARD));
/** Built-in: everything. Otherwise the explicit grant, or the right granted to all classes. */
const effective = (key: string, right: ClassRight) => builtin.value || rightsOf(key)[right] || (key !== WILDCARD && wildcard.value[right]);
const inherited = (key: string, right: ClassRight) => key !== WILDCARD && !rightsOf(key)[right] && wildcard.value[right];

function setRight(key: string, right: ClassRight, on: boolean) {
  const next = { ...rightsOf(key), [right]: on };
  // Create, edit and delete imply view; removing view removes them.
  if (on && right !== "view") next.view = true;
  if (!on && right === "view") Object.assign(next, NONE);
  grants.value = { ...grants.value, [key]: next };
}

function setGlobal(key: GlobalPermission, on: boolean) {
  globals.value = on ? [...globals.value, key] : globals.value.filter((g) => g !== key);
}

function classPermissions(): ClassPermission[] {
  return Object.entries(grants.value)
    .filter(([, r]) => r.view || r.create || r.edit || r.delete)
    .map(([k, r]) => ({ classId: k === WILDCARD ? null : k, ...r }));
}

const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() =>
  error.value instanceof ApiError ? error.value.details.filter((d) => !["name", "description", "requireMfa"].includes(d.field)) : [],
);
const holdsThis = computed(() => !!session.user?.profiles.some((p) => p.id === id.value));
/** Saving would lock the editor into two-factor set-up: say so before they click. */
const locksSelf = computed(() => requireMfa.value && !profile.data.value?.requireMfa && holdsThis.value && !session.session?.mfa.totpEnabled);

function formBody(): ProfileUpdateBody {
  return {
    name: name.value.trim(),
    description: description.value.trim() || null,
    globalPermissions: globals.value,
    classPermissions: classPermissions(),
    requireMfa: requireMfa.value,
  };
}

/** The fields that differ from the stored profile (on a new profile: from an empty one). */
const changes = computed(() => {
  // formBody() reads the form's refs; `initial` is replaced together with them in seed().
  const base: ProfileUpdateBody = isNew.value ? { name: "", description: null, globalPermissions: [], classPermissions: [], requireMfa: false } : initial;
  const changed = Object.keys(changedFields(formBody(), base));
  return builtin.value ? changed.filter((k) => k === "requireMfa").length : changed.length;
});
const dirty = computed(() => canManage.value && changes.value > 0);
const guard = useUnsavedGuard(() => dirty.value, () => t("admin.unsaved.leave"));

/** Back to the stored values. */
function discard() {
  seed(isNew.value ? undefined : profile.data.value);
  error.value = null;
  local.value = {};
}

async function submit() {
  if (!canManage.value) return;
  error.value = null;
  if (builtin.value) {
    try {
      const next = await update.mutateAsync({ id: id.value!, body: { requireMfa: requireMfa.value } });
      flash.show(t("admin.profile.saved", { name: next?.name ?? "" }));
      if (holdsThis.value) await session.refresh();
    } catch (e) {
      error.value = e;
    }
    return;
  }
  local.value = name.value.trim() ? {} : { name: t("common.required") };
  if (local.value.name) {
    document.getElementById("profile-name")?.focus();
    return;
  }
  const body = formBody();
  try {
    if (isNew.value) {
      const created = await create.mutateAsync({ ...body, name: body.name! });
      if (created) {
        flash.show(t("admin.profile.created", { name: created.name }));
        guard.allow();
        await router.push(`/admin/profiles/${created.id}`);
      }
      return;
    }
    const changed = changedFields(body, initial);
    if (Object.keys(changed).length === 0) {
      flash.show(t("common.nothingChanged"));
      return;
    }
    const next = await update.mutateAsync({ id: id.value!, body: changed });
    flash.show(t("admin.profile.saved", { name: next?.name ?? "" }));
    if (holdsThis.value) await session.refresh();
  } catch (e) {
    error.value = e;
  }
}

const deleting = ref(false);
const moreActions = computed<RowMenuItem[]>(() =>
  canManage.value && !builtin.value ? [{ label: t("admin.profile.delete.confirm"), danger: true, action: () => (deleting.value = true) }] : [],
);
function onDeleted() {
  guard.allow();
  void router.replace("/admin/profiles");
}

/** The class matrix's column groups: reading, and the rights that change CIs. */
const RIGHT_GROUPS: { key: "read" | "change"; rights: ClassRight[] }[] = [
  { key: "read", rights: ["view"] },
  { key: "change", rights: ["create", "edit", "delete"] },
];
const rightLabel = (r: ClassRight) => t(`admin.profile.right.${r}`);
/** Per cell: "edit on Server", "delete on all classes" (the labels the specs and screen readers use). */
const cellLabel = (r: ClassRight, cls: string | null) =>
  cls === null ? t("admin.profile.cell.all", { right: t(`admin.right.${r}`) }) : t("admin.profile.cell", { right: t(`admin.right.${r}`), name: cls });

const crumbs = computed(() => adminCrumbs("profiles", { label: isNew.value ? t("admin.crumb.new") : (profile.data.value?.name ?? "…") }));
const notFound = computed(() => {
  const e = profile.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs v-if="!isNew && (profile.isLoading.value || profile.isError.value)" :items="crumbs" />
  <LoadingState v-if="!isNew && profile.isLoading.value" :label="t('admin.profile.loading')" />
  <template v-else-if="!isNew && profile.isError.value">
    <EmptyState v-if="notFound" :title="t('admin.profile.notFound.title')">
      {{ t("admin.profile.notFound.body", { id: id ?? "" }) }}
      <template #actions><RouterLink class="btn" to="/admin/profiles">{{ t("admin.profile.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="profile.error.value" :on-retry="() => profile.refetch()" />
  </template>
  <template v-else>
    <div class="record-head record-head-plain">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="shield" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ isNew ? t("admin.profile.newTitle") : profile.data.value?.name }}</h1>
            </div>
            <p v-if="profile.data.value && !isNew" class="record-meta" data-testid="record-meta">
              <span v-if="builtin" class="badge">{{ t("admin.profiles.builtin") }}</span>
              <span v-if="profile.data.value.requireMfa" class="badge warn" :title="t('admin.profiles.mfaRequiredTitle')">{{ t("admin.profiles.mfaRequired") }}</span>
              <RouterLink v-if="session.can('users.manage')" class="badge record-class-chip" :to="{ path: '/admin/users', query: { profileId: profile.data.value.id } }">
                {{ t("admin.profile.users", { n: profile.data.value.userCount }) }}
              </RouterLink>
              <span v-else class="badge">{{ t("admin.profile.users", { n: profile.data.value.userCount }) }}</span>
              <span class="record-meta-line">
                <time :datetime="profile.data.value.createdAt" :title="formatDateTime(profile.data.value.createdAt)">
                  {{ t("record.meta.created", { when: formatDate(profile.data.value.createdAt) }) }}
                </time>
                <span class="sep" aria-hidden="true">·</span>
                <time :datetime="profile.data.value.updatedAt" :title="formatDateTime(profile.data.value.updatedAt)">
                  {{ t("record.meta.updated", { when: formatRelative(profile.data.value.updatedAt) }) }}
                </time>
              </span>
            </p>
          </div>
        </div>
        <div v-if="profile.data.value && !isNew && canManage" class="actions">
          <button type="button" class="btn" @click="cloning = profile.data.value">{{ t("admin.profiles.row.clone") }}</button>
          <RowMenu v-if="moreActions.length > 0" :label="t('record.actions.more')" :items="moreActions" large />
          <DeleteProfileDialog v-if="moreActions.length > 0" v-model:open="deleting" :profile="profile.data.value" @deleted="onDeleted" />
        </div>
      </div>
    </div>
    <div v-if="builtin" class="alert" role="note">{{ t("admin.profile.builtinNote") }}</div>
    <div v-else-if="readOnly" class="alert" role="note">{{ t("admin.profile.readOnlyNote") }}</div>
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />

    <form id="profile-form" class="stack" novalidate @submit.prevent="submit">
      <section class="panel" aria-labelledby="profile-title">
        <div class="panel-header"><h2 id="profile-title">{{ t("admin.profile.section.profile") }}</h2></div>
        <div class="panel-body form-grid">
          <FormField id="profile-name" :label="t('admin.profiles.col.name')" required :error="fieldErrors.name">
            <template #default="{ id: fid, invalid, describedBy }">
              <input :id="fid" v-model="name" v-autofocus="isNew" type="text" :readonly="readOnly" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
          <FormField id="profile-description" :label="t('groups.field.description')" wide :error="fieldErrors.description">
            <template #default="{ id: fid, invalid, describedBy }">
              <textarea :id="fid" v-model="description" rows="2" :readonly="readOnly" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
        </div>
      </section>

      <section class="panel" aria-labelledby="signin-title">
        <div class="panel-header"><h2 id="signin-title">{{ t("admin.profile.section.signIn") }}</h2></div>
        <div class="panel-body stack">
          <label class="checkbox-row">
            <input id="profile-requireMfa" v-model="requireMfa" type="checkbox" :disabled="!canManage" aria-describedby="profile-requireMfa-hint" />
            {{ t("admin.profile.requireMfa") }}
          </label>
          <p id="profile-requireMfa-hint" class="hint no-margin">{{ t("admin.profile.requireMfaHint") }}</p>
          <div v-if="locksSelf" class="alert alert-warn" role="note">{{ t("admin.profile.locksSelf") }}</div>
          <span v-if="fieldErrors.requireMfa" class="error">{{ fieldErrors.requireMfa }}</span>
        </div>
      </section>

      <section class="panel" aria-labelledby="global-title">
        <div class="panel-header"><h2 id="global-title">{{ t("admin.profile.section.global") }}</h2></div>
        <div class="panel-body">
          <ul class="check-list">
            <li v-for="g in GLOBAL_PERMISSIONS" :key="g.key">
              <label>
                <input
                  type="checkbox"
                  :checked="builtin || globals.includes(g.key)"
                  :disabled="readOnly"
                  @change="setGlobal(g.key, ($event.target as HTMLInputElement).checked)"
                />
                <span>{{ g.label }} <code class="muted">{{ g.key }}</code><span class="hint">{{ g.hint }}</span></span>
              </label>
            </li>
          </ul>
        </div>
      </section>

      <section class="panel" aria-labelledby="class-title">
        <div class="panel-header">
          <h2 id="class-title">{{ t("admin.profile.section.classes") }}</h2>
          <span class="muted">{{ t("admin.profile.classesHint") }}</span>
        </div>
        <LoadingState v-if="classes.isLoading.value" :label="t('admin.profile.loadingClasses')" />
        <div v-else-if="classes.isError.value" class="panel-body">
          <ErrorAlert :error="classes.error.value" :on-retry="() => classes.refetch()" />
        </div>
        <template v-else>
          <div class="table-wrap matrix-wrap" role="region" tabindex="0" :aria-label="t('admin.profile.matrix')">
            <table class="data list-table matrix">
              <caption class="sr-only">{{ t("admin.profile.section.classes") }}</caption>
              <colgroup><col class="matrix-name" /></colgroup>
              <colgroup v-for="g in RIGHT_GROUPS" :key="g.key" :span="g.rights.length" class="matrix-group" />
              <thead>
                <tr>
                  <th scope="col" rowspan="2" class="matrix-corner">{{ t("admin.profile.col.class") }}</th>
                  <th v-for="g in RIGHT_GROUPS" :key="g.key" scope="colgroup" :colspan="g.rights.length" class="matrix-group-head">
                    {{ t(`admin.profile.group.${g.key}`) }}
                  </th>
                </tr>
                <tr>
                  <template v-for="g in RIGHT_GROUPS" :key="g.key">
                    <th v-for="(r, i) in g.rights" :key="r" scope="col" :class="['check', { 'group-start': i === 0 }]">{{ rightLabel(r) }}</th>
                  </template>
                </tr>
              </thead>
              <tbody>
                <tr class="wildcard">
                  <th scope="row">
                    {{ t("admin.profile.allClasses") }} <span class="muted matrix-note">{{ t("admin.profile.allClassesNote") }}</span>
                  </th>
                  <template v-for="g in RIGHT_GROUPS" :key="g.key">
                    <td v-for="(r, i) in g.rights" :key="r" :class="['check', { 'group-start': i === 0 }]">
                      <input
                        type="checkbox"
                        :aria-label="cellLabel(r, null)"
                        :checked="effective(WILDCARD, r)"
                        :disabled="readOnly"
                        @change="setRight(WILDCARD, r, ($event.target as HTMLInputElement).checked)"
                      />
                    </td>
                  </template>
                </tr>
                <tr v-for="c in rows" :key="c.id">
                  <th scope="row">
                    <span class="name-badges">
                      <span dir="auto">{{ c.name }}</span>
                      <span v-if="!c.isActive" class="badge off">{{ t("admin.profile.inactive") }}</span>
                      <span v-if="c.isAbstract" class="badge warn" :title="t('admin.profile.abstractTitle')">{{ t("admin.profile.abstract") }}</span>
                    </span>
                  </th>
                  <template v-for="g in RIGHT_GROUPS" :key="g.key">
                    <td v-for="(r, i) in g.rights" :key="r" :class="['check', { 'group-start': i === 0 }]">
                      <input
                        type="checkbox"
                        :aria-label="cellLabel(r, c.name)"
                        :checked="effective(c.id, r)"
                        :disabled="readOnly || inherited(c.id, r)"
                        :title="inherited(c.id, r) ? t('admin.profile.inherited') : undefined"
                        @change="setRight(c.id, r, ($event.target as HTMLInputElement).checked)"
                      />
                    </td>
                  </template>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="unknownGrants.length > 0" class="panel-body muted no-margin">{{ t("admin.profile.unknownGrants", { n: unknownGrants.length }) }}</p>
        </template>
      </section>
    </form>

    <SaveBar v-if="canManage" :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
      <RouterLink class="btn" to="/admin/profiles">{{ t("common.cancel") }}</RouterLink>
      <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="profile-form" class="btn btn-primary" :disabled="pending">
        {{ pending ? t("common.saving") : isNew ? t("admin.profile.create") : t("common.saveChanges") }}
      </button>
    </SaveBar>
  </template>
  <CloneProfileDialog :profile="cloning" @close="cloning = null" />
</template>
