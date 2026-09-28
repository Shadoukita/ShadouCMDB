<script setup lang="ts">
import { computed, ref } from "vue";
import { useChangeOwnPassword } from "../../api/admin";
import { ApiError } from "../../api/client";
import ErrorAlert from "../../components/ErrorAlert.vue";
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
  if (!current.value) errs.currentPassword = "Required";
  if (pw.value.length < 12) errs.newPassword = "Too short";
  if (pw.value !== pw2.value) errs.confirm = "The passwords do not match";
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
    <div class="panel-header"><h2 id="own-pw-title">Password</h2></div>
    <div v-if="provider" class="panel-body">
      <p class="muted no-margin" data-testid="own-provider-credentials">
        You sign in through <strong>{{ provider.name }}</strong> and have no password in ShadouCMDB. Change your password there.
      </p>
    </div>
    <form v-else class="panel-body stack" novalidate @submit.prevent="submit">
      <p class="muted no-margin">
        Changing your password signs you out on every other browser and device. This session stays signed in.
      </p>
      <div v-if="done" class="alert" role="status">Password changed. Your other sessions were ended.</div>
      <ErrorAlert v-if="generalError" :error="generalError" title="Password not changed" />
      <div class="form-grid">
        <FormField id="own-current-password" label="Current password" required :error="fieldErrors.currentPassword">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="current" type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div class="form-grid">
        <FormField id="own-new-password" label="New password" required :error="fieldErrors.newPassword" hint="At least 12 characters">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="own-confirm-password" label="Repeat new password" required :error="fieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="pw2" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <div>
        <button type="submit" class="btn btn-primary" :disabled="change.isPending.value">{{ change.isPending.value ? "Changing…" : "Change password" }}</button>
      </div>
    </form>
  </section>
</template>
