<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { normaliseCode } from "../../api/mfa";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { safeRedirect } from "../../router";
import { useSessionStore } from "../../stores/session";

/**
 * Sign-in: username and password, then — for users with two-factor authentication —
 * a code from their authenticator app or a recovery code. After an expired session the
 * operator lands here with ?redirect=… and goes back there.
 */
useDocumentTitle("Sign in");
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const step = ref<"password" | "code">("password");
const username = ref("");
const password = ref("");
const code = ref("");
const useRecovery = ref(false);
const busy = ref(false);
const error = ref<unknown>(null);
/** Why the operator is back at the password form (the code step timed out or took too many wrong codes). */
const restarted = ref<string | null>(null);
const redirect = computed(() => safeRedirect(route.query.redirect));

const apiError = computed(() => (error.value instanceof ApiError ? error.value : null));
/** Wrong credentials and lockouts are expected answers, phrased for the operator; anything else shows in full. */
const known = computed(() => {
  const e = apiError.value;
  if (e?.code === "UNAUTHENTICATED") {
    if (step.value === "code") return useRecovery.value ? "That recovery code is wrong or already used." : "Wrong code. Enter the current code from your authenticator app.";
    return "Wrong username or password, or the account is disabled.";
  }
  if (e?.code === "RATE_LIMITED") return e.message || "Too many failed attempts. Wait a moment and try again.";
  if (e?.code === "VALIDATION_ERROR" && step.value === "code") return e.fieldErrors().code ?? e.message;
  return null;
});

async function submit() {
  if (!username.value.trim() || !password.value) return;
  busy.value = true;
  error.value = null;
  restarted.value = null;
  try {
    const result = await session.login({ username: username.value.trim(), password: password.value });
    password.value = "";
    if (result === "mfa") {
      code.value = "";
      useRecovery.value = false;
      step.value = "code";
      return;
    }
    await router.replace(redirect.value);
  } catch (e) {
    error.value = e;
    password.value = "";
  } finally {
    busy.value = false;
  }
}

async function submitCode() {
  const value = normaliseCode(code.value);
  if (!value) return;
  busy.value = true;
  error.value = null;
  try {
    await session.loginMfa(value);
    await router.replace(redirect.value);
  } catch (e) {
    code.value = "";
    // The challenge is gone (5 minutes, or too many wrong codes): the password is needed again.
    if (e instanceof ApiError && e.code === "UNAUTHENTICATED" && /expired/i.test(e.message)) {
      restart(e.message);
      return;
    }
    error.value = e;
  } finally {
    busy.value = false;
  }
}

function restart(reason: string | null = null) {
  step.value = "password";
  code.value = "";
  error.value = null;
  restarted.value = reason;
}

function toggleRecovery() {
  useRecovery.value = !useRecovery.value;
  code.value = "";
  error.value = null;
}
</script>

<template>
  <main class="bare">
    <form v-if="step === 'password'" class="bare-card" aria-labelledby="login-title" @submit.prevent="submit">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="login-title">Sign in</h1>
      <div v-if="session.expired" class="alert alert-warn" role="status">
        Your session has ended. Sign in again to continue where you left off.
      </div>
      <div v-if="restarted" class="alert alert-warn" role="status">{{ restarted }}</div>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" title="Could not sign in" />
      <div class="field">
        <label for="login-username">Username</label>
        <input id="login-username" v-model="username" v-autofocus type="text" autocomplete="username" required />
      </div>
      <div class="field">
        <label for="login-password">Password</label>
        <input id="login-password" v-model="password" v-autofocus="!!username" type="password" autocomplete="current-password" required />
      </div>
      <button type="submit" class="btn btn-primary block" :disabled="busy">{{ busy ? "Signing in…" : "Sign in" }}</button>
      <p class="hint">Lost access to every administrator account? Run <code>shadoucmdb create-admin</code> on the server.</p>
    </form>

    <form v-else class="bare-card" aria-labelledby="mfa-title" novalidate @submit.prevent="submitCode">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="mfa-title">Two-factor authentication</h1>
      <p>
        Signing in as <strong>{{ username }}</strong>.
        <template v-if="useRecovery">Enter one of the recovery codes you saved when you set up two-factor authentication. Each code works once.</template>
        <template v-else>Open your authenticator app and enter the 6-digit code shown for ShadouCMDB.</template>
      </p>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" title="Could not sign in" />
      <div v-if="!useRecovery" class="field">
        <label for="login-code">Authentication code</label>
        <input
          id="login-code"
          key="totp"
          v-model="code"
          v-autofocus
          class="mono code-input"
          type="text"
          inputmode="numeric"
          autocomplete="one-time-code"
          maxlength="10"
          spellcheck="false"
          required
        />
      </div>
      <div v-else class="field">
        <label for="login-recovery">Recovery code</label>
        <input
          id="login-recovery"
          key="recovery"
          v-model="code"
          v-autofocus
          class="mono"
          type="text"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          required
        />
      </div>
      <button type="submit" class="btn btn-primary block" :disabled="busy || !code.trim()">{{ busy ? "Verifying…" : "Verify" }}</button>
      <p class="hint">
        <button type="button" class="btn-link" @click="toggleRecovery">
          {{ useRecovery ? "Use a code from the authenticator app" : "Lost your device? Use a recovery code" }}
        </button>
        ·
        <button type="button" class="btn-link" @click="restart()">Sign in as someone else</button>
      </p>
      <p class="hint">No device and no recovery codes left? A user manager can reset your two-factor authentication.</p>
    </form>
  </main>
</template>

<style scoped>
.code-input {
  font-size: var(--fs-lg);
  letter-spacing: 0.2em;
}
</style>
