<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useCreateUser, useUpdateUser, useUser, type User, type UserUpdateBody } from "../../api/admin";
import { ApiError } from "../../api/client";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SaveBar from "../../components/SaveBar.vue";
import { t } from "../../i18n";
import { useDocumentTitle, useUnsavedGuard } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { formatDateTime, formatRelative } from "../../lib/format";
import { emailErrorMessage, signInStatusLabel, userEmailError } from "../../lib/people";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";
import ProfilePicker from "./ProfilePicker.vue";
import UserAccountActions from "./UserAccountActions.vue";

/**
 * Administration › Users › new / one user: profile fields and profiles; the account actions sit beside the form.
 * The title row is the record page's (design §2.7, audit A3): name, a meta line with the account's state, the
 * related views as secondary buttons and Delete in the `⋯` menu. Saving is the admin pages' one pattern: the
 * docked save bar, a toast on success, errors inline.
 */
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
useDocumentTitle(() => (isNew.value ? t("admin.users.new") : user.data.value?.username));

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
});

const FIELDS = ["username", "displayName", "email", "password", "isActive", "profileIds"];
const fieldErrors = computed<Record<string, string>>(() => {
  const api: Record<string, string> = error.value instanceof ApiError ? error.value.fieldErrors() : {};
  const email = error.value instanceof ApiError ? emailErrorMessage(error.value.details) : undefined;
  return { ...api, ...(email ? { email } : {}), ...local.value };
});
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field.split(".")[0])) : []));
const isSelf = computed(() => !!id.value && id.value === session.user?.id);
/** Set for an account created by an identity provider: its name, e-mail and profiles are overwritten at every sign-in. */
const provider = computed(() => (isNew.value ? null : (user.data.value?.identityProvider ?? null)));

/** The fields that differ from the stored user (on a new user: those filled in). */
const changes = computed(() => {
  const f = form.value;
  const b = isNew.value || !user.data.value ? blank() : fromUser(user.data.value);
  return (["username", "displayName", "email", "password", "confirm", "isActive"] as const).filter((k) => f[k] !== b[k]).length +
    (f.profileIds.slice().sort().join() !== b.profileIds.slice().sort().join() ? 1 : 0);
});
const dirty = computed(() => changes.value > 0);
const guard = useUnsavedGuard(() => dirty.value, () => t("admin.unsaved.leave"));

async function submit() {
  error.value = null;
  const f = form.value;
  const errs: Record<string, string> = {};
  if (!f.username.trim()) errs.username = t("common.required");
  if (!f.displayName.trim()) errs.displayName = t("common.required");
  // Required since SHAA-1505: it links the account to its Person CI. An account from before then may stay
  // without one here (its owner enters it at their next sign-in), so its other fields can still be changed.
  const emailError = userEmailError(f.email, !isNew.value && user.data.value?.email === null);
  if (emailError) errs.email = emailError;
  if (isNew.value) {
    if ([...f.password].length < 12) errs.password = t("admin.password.tooShort");
    if (f.password !== f.confirm) errs.confirm = t("admin.password.mismatch");
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(`user-${Object.keys(errs)[0]}`)?.focus();
    return;
  }
  const email = f.email.trim();
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
      flash.show(t("admin.user.created", { name: created.username }));
      guard.allow();
      await router.push(`/admin/users/${created.id}`);
      return;
    }
    const u = user.data.value!;
    const body: UserUpdateBody = {};
    if (f.username.trim() !== u.username) body.username = f.username.trim();
    if (f.displayName.trim() !== u.displayName) body.displayName = f.displayName.trim();
    // An incomplete account (an e-mail but no Person) is linked by sending its e-mail again.
    if (email !== (u.email ?? "") || (u.signInStatus === "person_missing" && email)) body.email = email;
    const before = u.profiles.map((p) => p.id).sort();
    const after = [...f.profileIds].sort();
    if (before.join() !== after.join()) body.profileIds = f.profileIds;
    if (Object.keys(body).length === 0) {
      flash.show(t("common.nothingChanged"));
      return;
    }
    const next = await update.mutateAsync({ id: u.id, body });
    flash.show(t("admin.user.saved", { name: next.username }));
    // Editing yourself can change what you may do.
    if (isSelf.value) await session.refresh();
  } catch (e) {
    error.value = e;
  }
}

/** Back to the stored values. */
function discard() {
  form.value = isNew.value || !user.data.value ? blank() : fromUser(user.data.value);
  error.value = null;
  local.value = {};
}

// Delete sits in the `⋯` menu; its dialog is the account actions'. Your own account cannot be deleted here.
const accountActions = ref<InstanceType<typeof UserAccountActions>>();
const moreActions = computed<RowMenuItem[]>(() =>
  isSelf.value ? [] : [{ label: t("admin.user.delete"), danger: true, action: () => accountActions.value?.openDelete() }],
);

const crumbs = computed(() => adminCrumbs("users", { label: isNew.value ? t("admin.crumb.new") : (user.data.value?.username ?? "…") }));
const notFound = computed(() => {
  const e = user.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs v-if="!isNew && (user.isLoading.value || user.isError.value)" :items="crumbs" />
  <LoadingState v-if="!isNew && user.isLoading.value" :label="t('admin.user.loading')" />
  <template v-else-if="!isNew && user.isError.value">
    <EmptyState v-if="notFound" :title="t('admin.user.notFound.title')">
      {{ t("admin.user.notFound.body", { id: id ?? "" }) }}
      <template #actions><RouterLink class="btn" to="/admin/users">{{ t("admin.user.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="user.error.value" :on-retry="() => user.refetch()" />
  </template>
  <template v-else>
    <div class="record-head record-head-plain">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="user" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ isNew ? t("admin.users.new") : user.data.value?.username }}</h1>
            </div>
            <p v-if="user.data.value && !isNew" class="record-meta" data-testid="record-meta">
              <span :class="['badge', user.data.value.isActive ? 'ok' : 'off']"
                ><span class="status-dot" aria-hidden="true" />{{ user.data.value.isActive ? t("common.active") : t("common.disabled") }}</span
              >
              <span v-if="user.data.value.isAdministrator" class="badge">{{ t("admin.user.administrator") }}</span>
              <span v-if="user.data.value.mfaEnabled" class="badge ok" :title="t('admin.user.mfaOnTitle')">{{ t("admin.user.mfaOn") }}</span>
              <span v-if="provider" class="badge" data-testid="user-provider">{{ t("admin.user.signsInWith", { name: provider.name }) }}</span>
              <span v-if="isSelf" class="badge">{{ t("admin.user.you") }}</span>
              <span
                v-if="user.data.value.signInStatus !== 'ready'"
                :class="['badge', user.data.value.signInStatus === 'person_missing' ? 'danger' : 'warn']"
                :title="user.data.value.signInStatus === 'person_missing' ? t('people.users.incompleteTitle') : t('people.users.emailRequiredTitle')"
                data-testid="user-sign-in-status"
              >
                {{ signInStatusLabel(user.data.value.signInStatus) }}
              </span>
              <span class="record-meta-line">
                <span dir="auto">{{ user.data.value.displayName }}</span>
                <span class="sep" aria-hidden="true">·</span>
                <time v-if="user.data.value.lastLoginAt" :datetime="user.data.value.lastLoginAt" :title="formatDateTime(user.data.value.lastLoginAt)">
                  {{ t("admin.user.lastSignIn", { when: formatRelative(user.data.value.lastLoginAt) }) }}
                </time>
                <span v-else>{{ t("admin.user.neverSignedIn") }}</span>
              </span>
            </p>
          </div>
        </div>
        <div v-if="user.data.value && !isNew" class="actions">
          <RouterLink class="btn" :to="{ path: '/admin/api-tokens', query: { userId: user.data.value.id } }">{{ t("admin.users.row.tokens") }}</RouterLink>
          <RouterLink v-if="session.can('audit.view')" class="btn" :to="{ path: '/admin/audit', query: { actorId: user.data.value.id } }">
            {{ t("admin.users.row.audit") }}
          </RouterLink>
          <RowMenu v-if="moreActions.length > 0" :label="t('record.actions.more')" :items="moreActions" large />
        </div>
      </div>
    </div>

    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="grid-2">
      <form id="user-form" class="panel" aria-labelledby="user-form-title" novalidate @submit.prevent="submit">
        <div class="panel-header"><h2 id="user-form-title">{{ t("admin.user.account") }}</h2></div>
        <div class="panel-body stack">
          <div v-if="provider" class="alert alert-warn" role="note" data-testid="provider-notice">
            <strong>{{ t("admin.user.provider.title", { name: provider.name }) }}</strong>
            <div>
              {{ t("admin.user.provider.body", { name: provider.name }) }}
              <template v-if="session.isAdministrator">
                <RouterLink :to="`/admin/identity-providers/${provider.id}`">{{ t("admin.user.provider.link", { name: provider.name }) }}</RouterLink>.
              </template>
              <template v-else>{{ t("admin.user.provider.adminTask") }}</template>
            </div>
          </div>
          <div class="form-grid">
            <FormField id="user-username" :label="t('admin.users.col.username')" required :error="fieldErrors.username" :hint="t('admin.user.usernameHint')">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.username" v-autofocus="isNew" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <FormField id="user-displayName" :label="t('admin.users.col.displayName')" required :error="fieldErrors.displayName" :hint="t('admin.user.displayNameHint')">
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.displayName" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <FormField
              id="user-email"
              :label="t('admin.users.col.email')"
              :required="isNew || user.data.value?.email !== null"
              :error="fieldErrors.email"
              :hint="user.data.value?.email === null && !isNew ? t('people.users.emailLegacyHint') : t('people.users.emailHint')"
            >
              <template #default="{ id: fid, invalid, describedBy }">
                <input :id="fid" v-model="form.email" type="email" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
            <template v-if="isNew">
              <div class="field">
                <span class="label">{{ t("admin.col.status") }}</span>
                <label class="checkbox-row"><input v-model="form.isActive" type="checkbox" /> {{ t("admin.user.activeCheckbox") }}</label>
              </div>
              <FormField id="user-password" :label="t('admin.password.label')" required :error="fieldErrors.password" :hint="t('admin.password.hint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.password" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="user-confirm" :label="t('admin.password.repeat')" required :error="fieldErrors.confirm">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.confirm" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </template>
          </div>
          <ProfilePicker v-model="form.profileIds" :error="fieldErrors.profileIds" />
        </div>
      </form>

      <div v-if="user.data.value && !isNew" class="stack">
        <section class="panel" aria-labelledby="user-facts-title">
          <div class="panel-header"><h2 id="user-facts-title">{{ t("admin.user.activity") }}</h2></div>
          <div class="panel-body">
            <dl class="props">
              <dt>{{ t("people.users.col.person") }}</dt>
              <dd data-testid="user-person">
                <RouterLink v-if="user.data.value.person" :to="`/cis/${user.data.value.person.id}`" dir="auto">{{ user.data.value.person.label }}</RouterLink>
                <template v-else-if="user.data.value.signInStatus === 'email_required'">{{ t("people.users.personAfterEmail") }}</template>
                <template v-else>{{ t("people.users.incompleteTitle") }}</template>
              </dd>
              <dt>{{ t("admin.users.col.lastLogin") }}</dt>
              <dd>{{ user.data.value.lastLoginAt ? formatDateTime(user.data.value.lastLoginAt) : t("admin.never") }}</dd>
              <dt>{{ t("admin.user.mfa") }}</dt>
              <dd>{{ user.data.value.mfaEnabled ? t("admin.user.mfaApp") : t("admin.users.mfaOff") }}</dd>
              <template v-if="provider">
                <dt>{{ t("admin.users.col.signIn") }}</dt>
                <dd>{{ provider.name }} ({{ provider.kind === "ldap" ? t("admin.user.kind.ldap") : t("admin.user.kind.oidc") }})</dd>
              </template>
              <template v-else>
                <dt>{{ t("admin.user.passwordChanged") }}</dt>
                <dd>{{ formatDateTime(user.data.value.passwordChangedAt) }}</dd>
              </template>
              <dt>{{ t("common.created") }}</dt>
              <dd>{{ formatDateTime(user.data.value.createdAt) }}</dd>
              <dt>{{ t("common.updated") }}</dt>
              <dd>{{ formatDateTime(user.data.value.updatedAt) }}</dd>
            </dl>
          </div>
        </section>
        <UserAccountActions ref="accountActions" :user="user.data.value" :is-self="isSelf" @deleted="guard.allow()" />
      </div>
    </div>

    <SaveBar :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
      <RouterLink class="btn" to="/admin/users">{{ t("common.cancel") }}</RouterLink>
      <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="user-form" class="btn btn-primary" :disabled="pending">
        {{ pending ? t("common.saving") : isNew ? t("admin.user.create") : t("common.saveChanges") }}
      </button>
    </SaveBar>
  </template>
</template>
