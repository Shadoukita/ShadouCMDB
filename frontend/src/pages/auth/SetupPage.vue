<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import BrandMark from "../../components/BrandMark.vue";
import { t, tAround } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";

/**
 * First-run setup, shown while GET /setup says no user exists: creates the first
 * administrator and signs them in. The API refuses it once any user exists.
 */
useDocumentTitle(() => t("auth.setup.documentTitle"));
const router = useRouter();
const session = useSessionStore();
const form = ref({ setupToken: "", username: "", displayName: "", email: "", password: "", confirm: "" });
const busy = ref(false);
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

const FIELDS = ["setupToken", "username", "displayName", "email", "password"];
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const intro = computed(() => tAround("auth.setup.intro", "profile"));
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  const f = form.value;
  const errs: Record<string, string> = {};
  if (!f.setupToken.trim()) errs.setupToken = t("common.required");
  if (!f.username.trim()) errs.username = t("common.required");
  if (!f.displayName.trim()) errs.displayName = t("common.required");
  if (!f.email.trim()) errs.email = t("common.required");
  if ([...f.password].length < 12) errs.password = t("auth.password.tooShort");
  if (f.password !== f.confirm) errs.confirm = t("auth.password.mismatch");
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(`setup-${Object.keys(errs)[0]}`)?.focus();
    return;
  }
  busy.value = true;
  try {
    await session.setup({
      username: f.username.trim(),
      displayName: f.displayName.trim(),
      email: f.email.trim(),
      password: f.password,
      setupToken: f.setupToken.trim(),
    });
    await router.replace("/");
  } catch (e) {
    error.value = e;
    // Someone else finished setup first: sign in instead.
    if (e instanceof ApiError && e.code === "CONFLICT") session.status = "anonymous";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <main class="bare">
    <form class="bare-card wide" aria-labelledby="setup-title" novalidate @submit.prevent="submit">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="setup-title">{{ t("auth.setup.title") }}</h1>
      <p class="muted">
        {{ intro[0] }}<strong>{{ t("auth.setup.introProfile") }}</strong>{{ intro[1] }}
      </p>
      <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
      <div v-if="error instanceof ApiError && error.code === 'CONFLICT'" class="alert">
        <RouterLink to="/login">{{ t("auth.setup.goToSignIn") }}</RouterLink>
      </div>
      <div class="form-grid">
        <FormField
          id="setup-setupToken"
          :label="t('auth.setup.token')"
          required
          :error="fieldErrors.setupToken"
          :hint="t('auth.setup.tokenHint')"
        >
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.setupToken" v-autofocus type="password" autocomplete="off" spellcheck="false" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <div />
        <FormField id="setup-username" required :label="t('auth.setup.username')" :error="fieldErrors.username" :hint="t('auth.setup.usernameHint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.username" type="text" autocomplete="username" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-displayName" required :label="t('auth.setup.displayName')" :error="fieldErrors.displayName" :hint="t('auth.setup.displayNameHint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.displayName" type="text" autocomplete="name" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-email" :label="t('auth.setup.email')" required :error="fieldErrors.email" :hint="t('people.setup.emailHint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.email" type="email" autocomplete="email" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <div />
        <FormField id="setup-password" required :label="t('auth.setup.password')" :error="fieldErrors.password" :hint="t('auth.password.minLengthHint')">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.password" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-confirm" :label="t('auth.setup.repeatPassword')" required :error="fieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.confirm" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <button type="submit" class="btn btn-primary" :disabled="busy">{{ busy ? t("auth.setup.submitting") : t("auth.setup.submit") }}</button>
    </form>
  </main>
</template>
