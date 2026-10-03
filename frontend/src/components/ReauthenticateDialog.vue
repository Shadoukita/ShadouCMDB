<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { ApiError } from "../api/client";
import { normaliseCode, useMfaStatus, useReauthenticate } from "../api/mfa";
import { t } from "../i18n";
import { reauthentication } from "../lib/reauthentication";
import FormField from "../pages/form/FormField.vue";
import { loginQuery } from "../router";
import { useSessionStore } from "../stores/session";
import ErrorAlert from "./ErrorAlert.vue";
import FormDialog from "./FormDialog.vue";

/**
 * Opens when a change to accounts, profiles, API tokens or identity providers answered 403
 * REAUTHENTICATION_REQUIRED (GH#498): the password, and a code once MFA is set up, open those
 * changes for 10 minutes. The change itself is not repeated: the page that sent it shows its own
 * error, and the user sends it again. An OIDC account has no password here and signs in again.
 */
const session = useSessionStore();
const router = useRouter();
const status = useMfaStatus();
const confirm = useReauthenticate();

const password = ref("");
const code = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

const provider = computed(() => session.user?.identityProvider ?? null);
const oidc = computed(() => provider.value?.kind === "oidc");
const directory = computed(() => provider.value?.kind === "ldap");
const needsCode = computed(() => !!status.data.value?.totpEnabled);
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const generalError = computed(() => {
  const e = error.value;
  if (e instanceof ApiError && e.code === "VALIDATION_ERROR" && e.details.every((d) => ["currentPassword", "code"].includes(d.field))) return null;
  return e;
});

watch(
  () => reauthentication.open,
  (open) => {
    if (!open) return;
    password.value = "";
    code.value = "";
    error.value = null;
    local.value = {};
    void status.refetch();
  },
);

function close() {
  reauthentication.open = false;
}

async function submit() {
  if (oidc.value) {
    close();
    const here = router.currentRoute.value;
    await session.logout().catch(() => undefined);
    await router.replace({ path: "/login", query: loginQuery(here) });
    return;
  }
  error.value = null;
  const errs: Record<string, string> = {};
  if (!password.value) errs.currentPassword = t("common.required");
  if (needsCode.value && !normaliseCode(code.value)) errs.code = t("common.required");
  local.value = errs;
  if (Object.keys(errs).length) return;
  try {
    await confirm.mutateAsync({
      currentPassword: password.value,
      ...(needsCode.value ? { code: normaliseCode(code.value) } : {}),
    });
    close();
  } catch (e) {
    error.value = e;
    code.value = "";
  }
}
</script>

<template>
  <FormDialog
    :open="reauthentication.open"
    :title="t('reauth.title')"
    :submit-label="oidc ? t('reauth.signInAgain') : t('reauth.confirm')"
    :busy="confirm.isPending.value"
    @submit="submit"
    @cancel="close"
  >
    <p>{{ oidc ? t("reauth.oidcBody") : t("reauth.body") }}</p>
    <ErrorAlert v-if="generalError" :error="generalError" />
    <template v-if="!oidc">
      <FormField
        id="reauth-currentPassword"
        :label="directory ? t('account.mfa.directoryPassword') : t('account.mfa.currentPassword')"
        required
        :error="fieldErrors.currentPassword"
        :hint="directory ? t('account.mfa.directoryPasswordHint', { provider: provider!.name }) : undefined"
      >
        <template #default="{ id, invalid, describedBy }">
          <input
            :id="id"
            v-model="password"
            type="password"
            autocomplete="current-password"
            :aria-invalid="invalid"
            :aria-describedby="describedBy"
          />
        </template>
      </FormField>
      <FormField
        v-if="needsCode"
        id="reauth-code"
        :label="t('account.mfa.code')"
        required
        :error="fieldErrors.code"
        :hint="t('account.mfa.codeHint')"
      >
        <template #default="{ id, invalid, describedBy }">
          <input
            :id="id"
            v-model="code"
            class="mono"
            inputmode="text"
            autocomplete="one-time-code"
            :aria-invalid="invalid"
            :aria-describedby="describedBy"
          />
        </template>
      </FormField>
    </template>
  </FormDialog>
</template>
