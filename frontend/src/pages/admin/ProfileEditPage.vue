<script setup lang="ts">
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
import LoadingState from "../../components/LoadingState.vue";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { formatDateTime, plural } from "../../lib/format";
import { CLASS_RIGHTS, GLOBAL_PERMISSIONS, type ClassRight } from "../../lib/permissions";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";
import CloneProfileDialog from "./CloneProfileDialog.vue";
import DeleteProfileButton from "./DeleteProfileButton.vue";

/**
 * Create or edit a permission profile: name, global permissions and the class
 * matrix (view/create/edit/delete per class, plus an "all classes" row that also
 * covers classes created later). The matrix rows come from the API's class list.
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
const flashText = computed(() => (id.value ? flash.forCi(id.value) : undefined));
useDocumentTitle(() => (isNew.value ? "New profile" : profile.data.value?.name));

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
const saved = ref<string | null>(null);
const cloning = ref<PermissionProfile | null>(null);

function seed(p: PermissionProfile | undefined) {
  name.value = p?.name ?? "";
  description.value = p?.description ?? "";
  globals.value = p ? [...p.globalPermissions] : [];
  requireMfa.value = p?.requireMfa ?? false;
  const g: Record<string, Rights> = {};
  for (const c of p?.classPermissions ?? []) g[c.classId ?? WILDCARD] = { view: c.view, create: c.create, edit: c.edit, delete: c.delete };
  grants.value = g;
}
watch(() => profile.data.value, seed, { immediate: true });
watch(id, () => {
  if (!id.value) seed(undefined);
  error.value = null;
  local.value = {};
  saved.value = null;
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

async function submit() {
  if (!canManage.value) return;
  error.value = null;
  saved.value = null;
  if (builtin.value) {
    try {
      const next = await update.mutateAsync({ id: id.value!, body: { requireMfa: requireMfa.value } });
      saved.value = `Saved ${next?.name ?? "the profile"}. Users holding it are affected on their next request.`;
      if (holdsThis.value) await session.refresh();
    } catch (e) {
      error.value = e;
    }
    return;
  }
  local.value = name.value.trim() ? {} : { name: "Required" };
  if (local.value.name) {
    document.getElementById("profile-name")?.focus();
    return;
  }
  const body: ProfileUpdateBody = {
    name: name.value.trim(),
    description: description.value.trim() || null,
    globalPermissions: globals.value,
    classPermissions: classPermissions(),
    requireMfa: requireMfa.value,
  };
  try {
    if (isNew.value) {
      const created = await create.mutateAsync({ ...body, name: body.name! });
      if (created) {
        flash.show(created.id, `Created profile ${created.name}.`);
        await router.push(`/admin/profiles/${created.id}`);
      }
      return;
    }
    const next = await update.mutateAsync({ id: id.value!, body });
    saved.value = `Saved ${next?.name ?? "the profile"}. Users holding it have the new permissions on their next request.`;
    if (holdsThis.value) await session.refresh();
  } catch (e) {
    error.value = e;
  }
}

const crumbs = computed(() => [
  { label: "Administration", to: "/admin" },
  { label: "Permission profiles", to: "/admin/profiles" },
  { label: isNew.value ? "New" : (profile.data.value?.name ?? "…") },
]);
const notFound = computed(() => {
  const e = profile.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && profile.isLoading.value" label="Loading profile…" />
  <template v-else-if="!isNew && profile.isError.value">
    <EmptyState v-if="notFound" title="Permission profile not found">
      No profile has the id <code>{{ id }}</code>. It may have been deleted.
      <template #actions><RouterLink class="btn" to="/admin/profiles">Back to profiles</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="profile.error.value" :on-retry="() => profile.refetch()" />
  </template>
  <form v-else novalidate @submit.prevent="submit">
    <div class="page-header">
      <div class="title">
        <h1>{{ isNew ? "New permission profile" : profile.data.value?.name }}</h1>
        <span v-if="builtin" class="badge">Built-in</span>
        <span v-if="profile.data.value" class="muted">
          {{ plural(profile.data.value.userCount, "user") }}
        </span>
      </div>
      <div v-if="profile.data.value && !isNew && session.can('profiles.manage')" class="actions">
        <button type="button" class="btn" @click="cloning = profile.data.value">Clone</button>
        <DeleteProfileButton v-if="!builtin" :profile="profile.data.value" />
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>
    <div v-if="builtin" class="alert" role="note">
      The built-in Administrator profile holds every permission, on every class, and cannot be deleted. Only its two-factor
      requirement can be changed. Clone it to start an editable profile from it.
    </div>
    <div v-else-if="readOnly" class="alert" role="note">You can view this profile. Changing it needs the <code>profiles.manage</code> permission.</div>
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div v-if="saved" class="alert" role="status">{{ saved }}</div>

    <section class="panel">
      <div class="panel-header"><h2>Profile</h2></div>
      <div class="panel-body form-grid">
        <FormField id="profile-name" label="Name" required :error="fieldErrors.name">
          <template #default="{ id: fid, invalid, describedBy }">
            <input :id="fid" v-model="name" v-autofocus="isNew" type="text" :readonly="readOnly" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="profile-description" label="Description" wide :error="fieldErrors.description">
          <template #default="{ id: fid, invalid, describedBy }">
            <textarea :id="fid" v-model="description" rows="2" :readonly="readOnly" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
    </section>

    <section class="panel" aria-labelledby="signin-title">
      <div class="panel-header"><h2 id="signin-title">Sign-in</h2></div>
      <div class="panel-body stack">
        <label class="checkbox-row">
          <input id="profile-requireMfa" v-model="requireMfa" type="checkbox" :disabled="!canManage" />
          Require two-factor authentication
        </label>
        <p class="hint" style="margin: 0">
          Users holding this profile must set up an authenticator app. Until they do, they can only sign in and set it up;
          everything else is refused. Their API tokens are refused too, unless the token was created from a session that
          completed two-factor sign-in.
        </p>
        <div v-if="locksSelf" class="alert alert-warn" role="note">
          You hold this profile and have not set up two-factor authentication. After saving you will be asked to set it up
          before you can continue.
        </div>
        <span v-if="fieldErrors.requireMfa" class="error">{{ fieldErrors.requireMfa }}</span>
      </div>
    </section>

    <section class="panel" aria-labelledby="global-title">
      <div class="panel-header"><h2 id="global-title">Global permissions</h2></div>
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
        <h2 id="class-title">Configuration item permissions by class</h2>
        <span class="muted">Create, edit and delete include view. A grant applies to exactly that class.</span>
      </div>
      <LoadingState v-if="classes.isLoading.value" label="Loading classes…" />
      <div v-else-if="classes.isError.value" class="panel-body">
        <ErrorAlert :error="classes.error.value" :on-retry="() => classes.refetch()" />
      </div>
      <div v-else class="table-wrap">
        <table class="data matrix">
          <thead>
            <tr>
              <th scope="col">Class</th>
              <th v-for="r in CLASS_RIGHTS" :key="r" scope="col" class="check">{{ r[0].toUpperCase() + r.slice(1) }}</th>
            </tr>
          </thead>
          <tbody>
            <tr class="wildcard">
              <th scope="row">
                All classes <span class="muted" style="font-weight: 400">— including classes added later</span>
              </th>
              <td v-for="r in CLASS_RIGHTS" :key="r" class="check">
                <input
                  type="checkbox"
                  :aria-label="`${r} on all classes`"
                  :checked="effective(WILDCARD, r)"
                  :disabled="readOnly"
                  @change="setRight(WILDCARD, r, ($event.target as HTMLInputElement).checked)"
                />
              </td>
            </tr>
            <tr v-for="c in rows" :key="c.id">
              <th scope="row">
                {{ c.name }}
                <span v-if="!c.isActive" class="badge off">inactive</span>
                <span v-if="c.isAbstract" class="badge warn" title="Abstract classes hold no CIs; grants do not pass to subclasses">abstract</span>
              </th>
              <td v-for="r in CLASS_RIGHTS" :key="r" class="check">
                <input
                  type="checkbox"
                  :aria-label="`${r} on ${c.name}`"
                  :checked="effective(c.id, r)"
                  :disabled="readOnly || inherited(c.id, r)"
                  :title="inherited(c.id, r) ? 'Granted by All classes' : undefined"
                  @change="setRight(c.id, r, ($event.target as HTMLInputElement).checked)"
                />
              </td>
            </tr>
          </tbody>
        </table>
        <p v-if="unknownGrants.length > 0" class="panel-body muted" style="margin: 0">
          This profile also grants rights on {{ unknownGrants.length }} class(es) not in the class list; saving keeps them.
        </p>
      </div>
    </section>

    <div v-if="profile.data.value && !isNew" class="muted" style="font-size: var(--fs-sm); margin-bottom: var(--sp-4)">
      Created {{ formatDateTime(profile.data.value.createdAt) }} · updated {{ formatDateTime(profile.data.value.updatedAt) }}
    </div>
    <div v-if="canManage" class="form-footer panel">
      <button type="submit" class="btn btn-primary" :disabled="pending">{{ pending ? "Saving…" : isNew ? "Create profile" : "Save changes" }}</button>
      <RouterLink class="btn" to="/admin/profiles">Cancel</RouterLink>
    </div>
  </form>
  <CloneProfileDialog :profile="cloning" @close="cloning = null" />
</template>
