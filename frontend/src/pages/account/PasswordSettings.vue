<script setup lang="ts">
import { computed, ref } from "vue";
import { useChangeOwnPassword } from "../../api/admin";
import { ApiError } from "../../api/client";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t, tAround } from "../../i18n";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";

/**
 * The signed-in user's own password: current + new + repeat. The API keeps this
 * session and ends the others. An identity provider's account has no password
 * here, so the form is replaced by a pointer to the provider.
 */
const session = useSessionStore();
const change = useChangeOwnPassword();
const provider = computed(() => session.user?.identityProvider ?? null);

const providerNote = computed(() => tAround("account.password.provider", "provider"));
const current = ref("");
const pw = ref("");
const pw2 = ref("");
const local = ref<Record<string, string>>({});
const done = ref(false);
const fieldErrors = computed(() => ({
  ...(change.error.value instanceof ApiError ? change.error.value.fieldErrors() : {}),
  ...local.value,
}));
/** Errors the fields below do not show: a lockout, a conflict, a network failure. */
const generalError = computed(() => {
  const e = change.error.value;
  if (!e) return null;
  if (e instanceof ApiError && e.code === "VALIDATION_ERROR" && e.details.every((d) => ["currentPassword", "newPassword"].includes(d.field))) return null;
  return e;
});

const FIELD_IDS: Record<string, string> = { currentPassword: "own-current-password", newPassword: "own-new-password", confirm: "own-confirm-password" };

function submit() {
  done.value = false;
  change.reset();
  const errs: Record<string, string> = {};
  if (!current.value) errs.currentPassword = t("common.required");
  if ([...pw.value].length < 12) errs.newPassword = t("auth.password.tooShort");
  if (pw.value !== pw2.value) errs.confirm = t("auth.password.mismatch");
  local.value = errs;
  const first = Object.keys(errs)[0];
  if (first) {
    document.getElementById(FIELD_IDS[first])?.focus();
    return;
  }
  change.mutate(
    { currentPassword: current.value, newPassword: pw.value },
    {
      onSuccess: () => {
        current.value = "";
        pw.value = "";
        pw2.value = "";
        done.value = true;
      },
      onError: () => {
        current.value = "";
      },
    },
  );
}
</script>

<template>
  <section class="panel" aria-labelledby="own-pw-title">
    <div class="panel-header"><h2 id="own-pw-title">{{ t("account.password.title") }}</h2></div>
    <div v-if="provider" class="panel-body">
      <p class="muted no-margin" data-testid="own-provider-credentials">
        {{ providerNote[0] }}<strong>{{ provider.name }}</strong>{{ providerNote[1] }}
      </p>
    </div>
    <form v-else class="panel-body stack" novalidate @submit.prevent="submit">
      <p class="muted no-margin">
        {{ t("account.password.intro") }}
      </p>
      <div v-if="done" class="alert alert-success" role="status">{{ t("account.password.changed") }}</div>
      <ErrorAlert v-if="generalError" :error="generalError" :title="t('account.password.failed')" />
      <div class="form-grid">
        <FormField id="own-current-password" :label="t('account.password.current')" required :error="fieldErrors.currentPassword">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="current" type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div class="form-grid">
        <FormField id="own-new-password" :label="t('account.password.new')" required :error="fieldErrors.newPassword" :hint="t('auth.password.minLengthHint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="own-confirm-password" :label="t('account.password.repeat')" required :error="fieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw2" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div>
        <button type="submit" class="btn btn-primary" :disabled="change.isPending.value">{{ change.isPending.value ? t("account.password.submitting") : t("account.password.submit") }}</button>
      </div>
    </form>
  </section>
</template>
