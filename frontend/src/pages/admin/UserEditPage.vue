<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useCreateUser, useUpdateUser, useUser, type User, type UserUpdateBody } from "../../api/admin";
import { ApiError } from "../../api/client";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { formatDateTime } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";
import ProfilePicker from "./ProfilePicker.vue";
import UserAccountActions from "./UserAccountActions.vue";

/** Administration › Users › new / one user: profile fields and profiles; the account actions sit beside the form. */
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const user = useUser(id);
const create = useCreateUser();
const update = useUpdateUser();
const pending = computed(() => create.isPending.value || update.isPending.value);
const flashText = computed(() => (id.value ? flash.forCi(id.value) : undefined));
useDocumentTitle(() => (isNew.value ? "New user" : user.data.value?.username));

interface Form {
  username: string;
  displayName: string;
  email: string;
  password: string;
  confirm: string;
  isActive: boolean;
  profileIds: string[];
}
const blank = (): Form => ({ username: "", displayName: "", email: "", password: "", confirm: "", isActive: true, profileIds: [] });
const fromUser = (u: User): Form => ({
  ...blank(),
  username: u.username,
  displayName: u.displayName,
  email: u.email ?? "",
  isActive: u.isActive,
  profileIds: u.profiles.map((p) => p.id),
});
const form = ref<Form>(blank());
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const saved = ref<string | null>(null);

// Seed the form from the record (and again after another save replaced it).
watch(
  () => user.data.value,
  (u) => {
    if (u) form.value = fromUser(u);
  },
  { immediate: true },
);
watch(id, () => {
  if (!id.value) form.value = blank();
  error.value = null;
  local.value = {};
  saved.value = null;
});

const FIELDS = ["username", "displayName", "email", "password", "isActive", "profileIds"];
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field.split(".")[0])) : []));
const isSelf = computed(() => !!id.value && id.value === session.user?.id);

async function submit() {
  error.value = null;
  saved.value = null;
  const f = form.value;
  const errs: Record<string, string> = {};
  if (!f.username.trim()) errs.username = "Required";
  if (!f.displayName.trim()) errs.displayName = "Required";
  if (isNew.value) {
    if (f.password.length < 12) errs.password = "Too short";
    if (f.password !== f.confirm) errs.confirm = "The passwords do not match";
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(`user-${Object.keys(errs)[0]}`)?.focus();
    return;
  }
  const email = f.email.trim() || null;
  try {
    if (isNew.value) {
      const created = await create.mutateAsync({
        username: f.username.trim(),
        displayName: f.displayName.trim(),
        email,
        password: f.password,
        isActive: f.isActive,
        profileIds: f.profileIds,
      });
      flash.show(created.id, `Created user ${created.username}.`);
      await router.push(`/admin/users/${created.id}`);
      return;
    }
    const u = user.data.value!;
    const body: UserUpdateBody = {};
    if (f.username.trim() !== u.username) body.username = f.username.trim();
    if (f.displayName.trim() !== u.displayName) body.displayName = f.displayName.trim();
    if (email !== u.email) body.email = email;
    const before = u.profiles.map((p) => p.id).sort();
    const after = [...f.profileIds].sort();
    if (before.join() !== after.join()) body.profileIds = f.profileIds;
    if (Object.keys(body).length === 0) {
      saved.value = "Nothing changed.";
      return;
    }
    const next = await update.mutateAsync({ id: u.id, body });
    saved.value = `Saved ${next.username}.`;
    // Editing yourself can change what you may do.
    if (isSelf.value) await session.refresh();
  } catch (e) {
    error.value = e;
  }
}

const crumbs = computed(() => [
  { label: "Administration", to: "/admin" },
  { label: "Users", to: "/admin/users" },
  { label: isNew.value ? "New" : (user.data.value?.username ?? "…") },
]);
const notFound = computed(() => {
  const e = user.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && user.isLoading.value" label="Loading user…" />
  <template v-else-if="!isNew && user.isError.value">
    <EmptyState v-if="notFound" title="User not found">
      No user has the id <code>{{ id }}</code>. It may have been deleted.
      <template #actions><RouterLink class="btn" to="/admin/users">Back to users</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="user.error.value" :on-retry="() => user.refetch()" />
  </template>
  <template v-else>
    <div class="page-header">
      <div class="title">
        <h1>{{ isNew ? "New user" : user.data.value?.username }}</h1>
        <template v-if="user.data.value && !isNew">
          <span v-if="user.data.value.isActive" class="badge ok">Active</span>
          <span v-else class="badge off">Disabled</span>
          <span v-if="user.data.value.isAdministrator" class="badge">Administrator</span>
          <span v-if="isSelf" class="badge">You</span>
        </template>
      </div>
      <div v-if="user.data.value && !isNew" class="actions">
        <RouterLink class="btn" :to="{ path: '/admin/api-tokens', query: { userId: user.data.value.id } }">API tokens of this user</RouterLink>
        <RouterLink v-if="session.can('audit.view')" class="btn" :to="{ path: '/admin/audit', query: { actorId: user.data.value.id } }">
          Changes by this user
        </RouterLink>
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>

    <div class="grid-2">
      <form class="panel" aria-labelledby="user-form-title" novalidate @submit.prevent="submit">
        <div class="panel-header"><h2 id="user-form-title">Account</h2></div>
        <div class="panel-body stack">
          <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
          <div v-if="saved" class="alert" role="status">{{ saved }}</div>
          <div class="form-grid">
            <FormField id="user-username" label="Username" required :error="fieldErrors.username" hint="Used to sign in; letters, digits and . _ @ -">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.username" v-autofocus="isNew" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <FormField id="user-displayName" label="Display name" required :error="fieldErrors.displayName" hint="Shown in the audit log">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.displayName" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <FormField id="user-email" label="Email" :error="fieldErrors.email">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.email" type="email" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <template v-if="isNew">
              <div class="field">
                <span class="label">Status</span>
                <label class="checkbox-row"><input v-model="form.isActive" type="checkbox" /> Active (can sign in)</label>
              </div>
              <FormField id="user-password" label="Password" required :error="fieldErrors.password" hint="At least 12 characters">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.password" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="user-confirm" label="Repeat password" required :error="fieldErrors.confirm">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.confirm" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </template>
          </div>
          <ProfilePicker v-model="form.profileIds" :error="fieldErrors.profileIds" />
        </div>
        <div class="form-footer">
          <button type="submit" class="btn btn-primary" :disabled="pending">
            {{ pending ? "Saving…" : isNew ? "Create user" : "Save changes" }}
          </button>
          <RouterLink class="btn" to="/admin/users">{{ isNew ? "Cancel" : "Back to users" }}</RouterLink>
        </div>
      </form>

      <div v-if="user.data.value && !isNew" class="stack">
        <section class="panel" aria-labelledby="user-facts-title">
          <div class="panel-header"><h2 id="user-facts-title">Activity</h2></div>
          <div class="panel-body">
            <dl class="props">
              <dt>Last sign-in</dt>
              <dd>{{ user.data.value.lastLoginAt ? formatDateTime(user.data.value.lastLoginAt) : "Never" }}</dd>
              <dt>Password changed</dt>
              <dd>{{ formatDateTime(user.data.value.passwordChangedAt) }}</dd>
              <dt>Created</dt>
              <dd>{{ formatDateTime(user.data.value.createdAt) }}</dd>
              <dt>Updated</dt>
              <dd>{{ formatDateTime(user.data.value.updatedAt) }}</dd>
            </dl>
          </div>
        </section>
        <UserAccountActions :user="user.data.value" :is-self="isSelf" />
      </div>
    </div>
  </template>
</template>
