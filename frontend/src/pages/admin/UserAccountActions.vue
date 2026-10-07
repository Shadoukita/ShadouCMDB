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
 * are the provider's business, so those panels are replaced by a pointer to it. Your own password and second factor
 * are not reset here either (the API answers 409): My account changes them and asks for the current password.
 */
const props = defineProps<{ user: User; isSelf: boolean }>();
/** Emitted just before the page leaves for the users list after a delete. */
const emit = defineEmits<{ deleted: [] }>();
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
      flash.show(`${t("users.deleted", { name: username })} ${services}`);
      emit("deleted");
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
  if ([...pw.value].length < 12) errs.password = t("admin.password.tooShort");
  if (pw.value !== pw2.value) errs.confirm = t("admin.password.mismatch");
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

/** Delete is offered in the page's `⋯` menu; the dialog stays here, with the other account actions. */
defineExpose({ openDelete });
</script>

<template>
  <section v-if="provider" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">{{ t("admin.account.providerTitle") }}</h2></div>
    <div class="panel-body">
      <p class="muted no-margin" data-testid="provider-credentials">{{ t("admin.account.providerBody", { user: user.username, name: provider.name }) }}</p>
    </div>
  </section>
  <section v-if="!provider && isSelf" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">{{ t("admin.password.label") }}</h2></div>
    <div class="panel-body">
      <p class="muted no-margin" data-testid="self-password">
        {{ t("admin.account.selfPassword") }} <RouterLink to="/account">{{ t("admin.account.myAccount") }}</RouterLink>.
      </p>
    </div>
  </section>
  <section v-if="!provider && !isSelf" class="panel" aria-labelledby="pw-title">
    <div class="panel-header"><h2 id="pw-title">{{ t("admin.account.resetPassword") }}</h2></div>
    <form class="panel-body stack" novalidate @submit.prevent="resetPassword">
      <p class="muted no-margin">{{ t("admin.account.resetPasswordHint", { user: user.username }) }}</p>
      <div v-if="pwDone" class="alert alert-success" role="status">{{ t("admin.account.passwordChanged", { user: user.username }) }}</div>
      <ErrorAlert v-if="setPassword.isError.value && !pwFieldErrors.password" :error="setPassword.error.value" :title="t('admin.account.passwordNotChanged')" />
      <div class="form-grid">
        <FormField id="reset-password" :label="t('admin.account.newPassword')" required :error="pwFieldErrors.password" :hint="t('admin.password.hint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="reset-confirm" :label="t('admin.account.repeatNewPassword')" required :error="pwFieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw2" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div><button type="submit" class="btn" :disabled="setPassword.isPending.value">{{ t("admin.account.setPassword") }}</button></div>
    </form>
  </section>

  <section v-if="!provider || user.mfaEnabled" class="panel" aria-labelledby="mfa-admin-title">
    <div class="panel-header"><h2 id="mfa-admin-title">{{ t("admin.user.mfa") }}</h2></div>
    <div class="panel-body stack">
      <div v-if="mfaDone" class="alert alert-success" role="status">{{ t("admin.account.mfaResetDone", { user: user.username }) }}</div>
      <p class="muted no-margin">
        {{ user.mfaEnabled ? t("admin.account.mfaOn", { user: user.username }) : t("admin.account.mfaOff", { user: user.username }) }}
      </p>
      <p v-if="isSelf && user.mfaEnabled" class="muted no-margin">
        {{ t("admin.account.selfMfa") }} <RouterLink to="/account">{{ t("admin.account.myAccount") }}</RouterLink>.
      </p>
      <div><button type="button" class="btn" :disabled="!user.mfaEnabled || isSelf" @click="openResetMfa">{{ t("admin.account.resetMfa") }}</button></div>
    </div>
  </section>

  <section class="panel" aria-labelledby="access-title">
    <div class="panel-header"><h2 id="access-title">{{ t("admin.account.access") }}</h2></div>
    <div class="panel-body stack">
      <p v-if="isSelf" class="muted no-margin">{{ t("admin.account.selfAccess") }}</p>
      <div class="actions">
        <button v-if="user.isActive" type="button" class="btn" :disabled="isSelf" @click="openToggle">{{ t("admin.account.disable") }}</button>
        <button v-else type="button" class="btn" @click="openToggle">{{ t("admin.account.enable") }}</button>
      </div>
      <p class="muted no-margin">{{ isSelf ? t("admin.account.preferDisable") : t("admin.account.preferDisableDelete") }}</p>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming === 'toggle'"
    :title="user.isActive ? t('admin.account.disableTitle', { user: user.username }) : t('admin.account.enableTitle', { user: user.username })"
    :confirm-label="user.isActive ? t('admin.account.disable') : t('admin.account.enable')"
    :busy="update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmToggle"
  >
    <ErrorAlert v-if="toggleError" :error="toggleError" :title="user.isActive ? t('admin.account.notDisabled') : t('admin.account.notEnabled')" />
    <p v-if="user.isActive"><strong dir="auto">{{ user.displayName }}</strong> ({{ user.username }}) {{ t("admin.account.disableBody") }}</p>
    <p v-else-if="provider"><strong dir="auto">{{ user.displayName }}</strong> ({{ user.username }}) {{ t("admin.account.enableBodyProvider", { name: provider.name }) }}</p>
    <p v-else><strong dir="auto">{{ user.displayName }}</strong> ({{ user.username }}) {{ t("admin.account.enableBody") }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'mfa'"
    :title="t('admin.account.resetMfaTitle', { user: user.username })"
    :confirm-label="t('admin.account.resetMfa')"
    :busy="resetMfa.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmResetMfa"
  >
    <ErrorAlert v-if="resetMfa.isError.value" :error="resetMfa.error.value" :title="t('admin.account.mfaNotReset')" />
    <p>{{ t("admin.account.resetMfaBody", { name: user.displayName, user: user.username }) }}</p>
    <p>{{ t("admin.account.resetMfaWarning") }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'delete'"
    :title="t('admin.account.deleteTitle', { user: user.username })"
    :confirm-label="t('admin.user.delete')"
    :busy="del.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmDelete"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('groups.delete.failed')" />
    <p><strong dir="auto">{{ user.displayName }}</strong> ({{ user.username }}) {{ t("admin.account.deleteBody") }}</p>
    <p v-if="user.profiles.length > 0">{{ t("admin.account.deleteProfiles", { profiles: user.profiles.map((p) => p.name).join(", ") }) }}</p>
    <p data-testid="user-delete-services">{{ t("users.delete.services") }}</p>
  </ConfirmDialog>
</template>
