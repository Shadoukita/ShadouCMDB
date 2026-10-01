<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import BrandMark from "../../components/BrandMark.vue";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";

/**
 * First-run setup, shown while GET /setup says no user exists: creates the first
 * administrator and signs them in. The API refuses it once any user exists.
 */
useDocumentTitle("First-run setup");
const router = useRouter();
const session = useSessionStore();
const form = ref({ setupToken: "", username: "", displayName: "", email: "", password: "", confirm: "" });
const busy = ref(false);
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

const FIELDS = ["setupToken", "username", "displayName", "email", "password"];
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  const f = form.value;
  const errs: Record<string, string> = {};
  if (!f.setupToken.trim()) errs.setupToken = "Required";
  if (!f.username.trim()) errs.username = "Required";
  if (!f.displayName.trim()) errs.displayName = "Required";
  if ([...f.password].length < 12) errs.password = "Too short";
  if (f.password !== f.confirm) errs.confirm = "The passwords do not match";
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
      email: f.email.trim() || null,
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
      <h1 id="setup-title">Welcome — create the first administrator</h1>
      <p class="muted">
        No user exists yet. The account you create here holds the built-in <strong>Administrator</strong> profile: it can
        manage users, permission profiles and every configuration item. You can add more users afterwards under
        Administration.
      </p>
      <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
      <div v-if="error instanceof ApiError && error.code === 'CONFLICT'" class="alert">
        <RouterLink to="/login">Go to sign-in</RouterLink>
      </div>
      <div class="form-grid">
        <FormField
          id="setup-setupToken"
          label="Setup token"
          required
          :error="fieldErrors.setupToken"
          hint="The one-time token the server wrote to its setup token file when it started (or to its log, if it has no such file)"
        >
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.setupToken" v-autofocus type="password" autocomplete="off" spellcheck="false" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <div />
        <FormField id="setup-username" label="Username" required :error="fieldErrors.username" hint="Letters, digits and . _ @ -">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.username" type="text" autocomplete="username" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-displayName" label="Display name" required :error="fieldErrors.displayName" hint="Shown in the header and the audit log">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.displayName" type="text" autocomplete="name" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-email" label="Email" :error="fieldErrors.email">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.email" type="email" autocomplete="email" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <div />
        <FormField id="setup-password" label="Password" required :error="fieldErrors.password" hint="At least 12 characters">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.password" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
        <FormField id="setup-confirm" label="Repeat password" required :error="fieldErrors.confirm">
          <template #default="{ id, invalid, describedBy }">
            <input :id="id" v-model="form.confirm" type="password" autocomplete="new-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
          </template>
        </FormField>
      </div>
      <button type="submit" class="btn btn-primary" :disabled="busy">{{ busy ? "Creating…" : "Create administrator and sign in" }}</button>
    </form>
  </main>
</template>
