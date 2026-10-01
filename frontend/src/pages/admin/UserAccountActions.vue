<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { useDeleteUser, useSetUserPassword, useUpdateUser, type User } from "../../api/admin";
import { ApiError } from "../../api/client";
import { useResetUserMfa } from "../../api/mfa";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";

/**
 * Disable/enable, reset password, reset two-factor and delete. Each says what happens to the user's sessions.
 * An identity provider's account has no password here (the API answers 409): its password and second factor
 * are the provider's business, so those panels are replaced by a pointer to it.
 */
const props = defineProps<{ user: User; isSelf: boolean }>();
const provider = computed(() => props.user.identityProvider);
const router = useRouter();
const update = useUpdateUser();
const setPassword = useSetUserPassword();
const del = useDeleteUser();
const resetMfa = useResetUserMfa();

const confirming = ref<"toggle" | "delete" | "mfa" | null>(null);
const mfaDone = ref(false);

function openResetMfa() {
  resetMfa.reset();
  mfaDone.value = false;
  confirming.value = "mfa";
}

function confirmResetMfa() {
  resetMfa.mutate(props.user, {
    onSuccess: () => {
      confirming.value = null;
      mfaDone.value = true;
    },
  });
}
const toggleError = ref<unknown>(null);

async function confirmToggle() {
  try {
    await update.mutateAsync({ id: props.user.id, body: { isActive: !props.user.isActive } });
    confirming.value = null;
  } catch (e) {
    toggleError.value = e;
  }
}

function openToggle() {
  toggleError.value = null;
  confirming.value = "toggle";
}

function openDelete() {
  del.reset();
  confirming.value = "delete";
}

const flash = useFlashStore();

/** The users list then says how many business services lost this user as owner (§4.10). */
function confirmDelete() {
  const username = props.user.username;
  del.mutate(props.user.id, {
    onSuccess: (res) => {
      const n = res?.affectedServices;
      const services = typeof n === "number" ? t("users.deleted.services", { n }) : t("users.deleted.servicesWithheld");
      flash.show("users", `${t("users.deleted", { name: username })} ${services}`);
      router.replace("/admin/users");
    },
  });
}

// Reset password
const pw = ref("");
const pw2 = ref("");
const pwLocal = ref<Record<string, string>>({});
const pwDone = ref(false);
const pwFieldErrors = computed(() => ({
  ...(setPassword.error.value instanceof ApiError ? setPassword.error.value.fieldErrors() : {}),
  ...pwLocal.value,
}));

function resetPassword() {
  pwDone.value = false;
  setPassword.reset();
  const errs: Record<string, string> = {};
  if ([...pw.value].length < 12) errs.password = "Too short";
  if (pw.value !== pw2.value) errs.confirm = "The passwords do not match";
  pwLocal.value = errs;
  if (Object.keys(errs).length > 0) return;
  setPassword.mutate(
    { id: props.user.id, password: pw.value },
    {
      onSuccess: () => {
        pw.value = "";
        pw2.value = "";
        pwDone.value = true;
      },
    },
  );
}
</script>

<template>
  <section v-if="provider" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">Password and two-factor</h2></div>
    <div class="panel-body">
      <p class="muted no-margin" data-testid="provider-credentials">
        {{ user.username }} signs in through <strong>{{ provider.name }}</strong> and has no password in ShadouCMDB.
        Passwords and two-factor authentication are managed there.
      </p>
    </div>
  </section>
  <section v-if="!provider && isSelf" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">Password</h2></div>
    <div class="panel-body">
      <p class="muted no-margin" data-testid="self-password">
        This is you: change your own password under <RouterLink to="/account">My account</RouterLink>. It asks for your
        current password.
      </p>
    </div>
  </section>
  <section v-if="!provider && !isSelf" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">Reset password</h2></div>
    <form class="panel-body stack" novalidate @submit.prevent="resetPassword">
      <p class="muted" style="margin: 0">
        Sets a new password for {{ user.username }}, signs them out everywhere and revokes their API tokens.
      </p>
      <div v-if="pwDone" class="alert" role="status">Password changed. {{ user.username }}'s sessions were ended.</div>
      <ErrorAlert v-if="setPassword.isError.value && !pwFieldErrors.password" :error="setPassword.error.value" title="Password not changed" />
      <div class="form-grid">
        <FormField id="reset-password" label="New password" required :error="pwFieldErrors.password" hint="At least 12 characters">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="reset-confirm" label="Repeat new password" required :error="pwFieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw2" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div><button type="submit" class="btn" :disabled="setPassword.isPending.value">Set new password</button></div>
    </form>
  </section>

  <section v-if="!provider || user.mfaEnabled" class="panel" aria-labelledby="mfa-admin-title">
    <div class="panel-header"><h2 id="mfa-admin-title">Two-factor authentication</h2></div>
    <div class="panel-body stack">
      <div v-if="mfaDone" class="alert" role="status">
        Two-factor authentication reset. {{ user.username }} signs in with their password only, or sets it up again if a profile requires it.
      </div>
      <p class="muted" style="margin: 0">
        <template v-if="user.mfaEnabled">
          {{ user.username }} signs in with a password and a code from an authenticator app. If they lost their device and
          their recovery codes, reset it.
        </template>
        <template v-else>{{ user.username }} has not set up two-factor authentication.</template>
      </p>
      <p v-if="isSelf && user.mfaEnabled" class="muted" style="margin: 0">
        This is you: manage your own authenticator under <RouterLink to="/account">My account</RouterLink>.
      </p>
      <div><button type="button" class="btn" :disabled="!user.mfaEnabled || isSelf" @click="openResetMfa">Reset two-factor</button></div>
    </div>
  </section>

  <section class="panel" aria-labelledby="danger-title">
    <div class="panel-header"><h2 id="danger-title">Access</h2></div>
    <div class="panel-body stack">
      <p v-if="isSelf" class="muted" style="margin: 0">
        This is your own account. Another user manager has to disable or delete it, so you cannot lock yourself out.
      </p>
      <div class="actions">
        <button v-if="user.isActive" type="button" class="btn" :disabled="isSelf" @click="openToggle">Disable account</button>
        <button v-else type="button" class="btn" @click="openToggle">Enable account</button>
        <button type="button" class="btn btn-danger" :disabled="isSelf" @click="openDelete">Delete user</button>
      </div>
      <p class="muted" style="margin: 0">Prefer disabling: a disabled user keeps their name on past changes and can be enabled again.</p>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming === 'toggle'"
    :title="user.isActive ? `Disable ${user.username}?` : `Enable ${user.username}?`"
    :confirm-label="user.isActive ? 'Disable account' : 'Enable account'"
    :busy="update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmToggle"
  >
    <ErrorAlert v-if="toggleError" :error="toggleError" :title="user.isActive ? 'Not disabled' : 'Not enabled'" />
    <p v-if="user.isActive">
      <strong>{{ user.displayName }}</strong> ({{ user.username }}) will be signed out everywhere and cannot sign in until
      the account is enabled again. Their profiles and history are kept.
    </p>
    <p v-else-if="provider">
      <strong>{{ user.displayName }}</strong> ({{ user.username }}) will be able to sign in again through {{ provider.name }},
      as long as their groups there still map to a profile.
    </p>
    <p v-else><strong>{{ user.displayName }}</strong> ({{ user.username }}) will be able to sign in again with their current password.</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'mfa'"
    :title="`Reset two-factor authentication for ${user.username}?`"
    confirm-label="Reset two-factor"
    :busy="resetMfa.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmResetMfa"
  >
    <ErrorAlert v-if="resetMfa.isError.value" :error="resetMfa.error.value" title="Two-factor authentication not reset" />
    <p>
      Deletes the authenticator and the recovery codes of <strong>{{ user.displayName }}</strong> ({{ user.username }}). They
      then sign in with their password only. If a permission profile they hold requires two-factor authentication, they
      must set it up again before they can continue working.
    </p>
    <p>Only do this after you have confirmed their identity: it removes the second sign-in factor. The reset is audited.</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'delete'"
    :title="`Delete user ${user.username}?`"
    confirm-label="Delete user"
    :busy="del.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmDelete"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Delete failed" />
    <p>
      <strong>{{ user.displayName }}</strong> ({{ user.username }}) will be removed and signed out everywhere. This cannot be
      undone. The audit log keeps their name on the changes they made.
    </p>
    <p v-if="user.profiles.length > 0">
      They hold: <strong>{{ user.profiles.map((p) => p.name).join(", ") }}</strong>. The profiles themselves are not deleted.
    </p>
    <p data-testid="user-delete-services">{{ t("users.delete.services") }}</p>
  </ConfirmDialog>
</template>
