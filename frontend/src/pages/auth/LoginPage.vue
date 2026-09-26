<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { safeRedirect } from "../../router";
import { useSessionStore } from "../../stores/session";

/** Sign-in. After an expired session the operator lands here with ?redirect=… and goes back there. */
useDocumentTitle("Sign in");
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const username = ref("");
const password = ref("");
const busy = ref(false);
const error = ref<unknown>(null);
const redirect = computed(() => safeRedirect(route.query.redirect));

const apiError = computed(() => (error.value instanceof ApiError ? error.value : null));
/** Wrong credentials and lockouts are expected answers, phrased for the operator; anything else shows in full. */
const known = computed(() => {
  const e = apiError.value;
  if (e?.code === "UNAUTHENTICATED") return "Wrong username or password, or the account is disabled.";
  if (e?.code === "RATE_LIMITED") return e.message || "Too many failed attempts. Wait a moment and try again.";
  return null;
});

async function submit() {
  if (!username.value.trim() || !password.value) return;
  busy.value = true;
  error.value = null;
  try {
    await session.login({ username: username.value.trim(), password: password.value });
    await router.replace(redirect.value);
  } catch (e) {
    error.value = e;
    password.value = "";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <main class="bare">
    <form class="bare-card" aria-labelledby="login-title" @submit.prevent="submit">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="login-title">Sign in</h1>
      <div v-if="session.expired" class="alert alert-warn" role="status">
        Your session has ended. Sign in again to continue where you left off.
      </div>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" title="Could not sign in" />
      <div class="field">
        <label for="login-username">Username</label>
        <input id="login-username" v-model="username" v-autofocus type="text" autocomplete="username" required />
      </div>
      <div class="field">
        <label for="login-password">Password</label>
        <input id="login-password" v-model="password" type="password" autocomplete="current-password" required />
      </div>
      <button type="submit" class="btn btn-primary block" :disabled="busy">{{ busy ? "Signing in…" : "Sign in" }}</button>
      <p class="hint">Lost access to every administrator account? Run <code>shadoucmdb create-admin</code> on the server.</p>
    </form>
  </main>
</template>
