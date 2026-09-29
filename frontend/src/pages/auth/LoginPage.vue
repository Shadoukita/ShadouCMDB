<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { oidcStartHref, useSignInOptions } from "../../api/identityProviders";
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
 *
 * Enterprise sign-in: one button per enabled OIDC provider (a browser navigation to the
 * API, which comes back to ?redirect's path, or to /login?ssoError=<code> on a problem).
 * Directory (LDAP / AD) accounts use the username/password form.
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
const options = useSignInOptions();
const oidc = computed(() => options.data.value?.oidc ?? []);
const directory = computed(() => !!options.data.value?.directory);

/** Why the last OIDC sign-in did not go through; the server never says more than the code. */
const SSO_ERRORS: Record<string, string> = {
  expired: "The sign-in took too long or was started in another browser tab. Start again.",
  cancelled: "The sign-in was cancelled at the identity provider.",
  failed: "The identity provider's answer could not be verified. Start again; if it keeps failing, ask an administrator to check the provider settings.",
  unavailable: "The identity provider could not be reached or is disabled. Try again later, or sign in with a local account.",
  not_configured: "Single sign-on is not fully set up on this server (PUBLIC_URL is missing). Ask an administrator.",
  not_authorised: "None of your groups gives access to ShadouCMDB. Ask an administrator for access.",
  account_conflict: "A ShadouCMDB account with your username already exists and does not belong to this identity provider. Ask an administrator to resolve the conflict.",
  account_disabled: "Your ShadouCMDB account is disabled. Ask an administrator.",
  invalid_username: "Your identity provider did not send a usable username. Ask an administrator to check the provider settings.",
  last_administrator: "Signing in would leave ShadouCMDB without an active administrator, because your groups no longer map to the Administrator profile. Ask another administrator to check the group mappings.",
  mfa_not_enforced: "Your identity provider did not confirm a second factor, which your access to ShadouCMDB requires. Sign in again using multi-factor authentication, or ask an administrator to check the provider's MFA settings.",
};
const ssoCode = computed(() => (typeof route.query.ssoError === "string" ? route.query.ssoError : null));
const ssoError = computed(() =>
  ssoCode.value ? (SSO_ERRORS[ssoCode.value] ?? `Single sign-on failed (${ssoCode.value}). Try again, or ask an administrator.`) : null,
);
const startHref = (startUrl: string) => oidcStartHref(startUrl, redirect.value);

const apiError = computed(() => (error.value instanceof ApiError ? error.value : null));
/** Wrong credentials and lockouts are expected answers, phrased for the operator; anything else shows in full. */
const known = computed(() => {
  const e = apiError.value;
  if (e?.code === "UNAUTHENTICATED") {
    if (step.value === "code") return useRecovery.value ? "That recovery code is wrong or already used." : "Wrong code. Enter the current code from your authenticator app.";
    return "Wrong username or password, or the account is disabled.";
  }
  if (e?.code === "RATE_LIMITED") return e.message || "Too many failed attempts. Wait a moment and try again.";
  if (e?.code === "IDENTITY_PROVIDER_UNAVAILABLE") {
    return "The directory (LDAP / Active Directory) could not be reached, so directory accounts cannot sign in right now. Local accounts still work. Try again later or tell an administrator.";
  }
  if (e?.code === "VALIDATION_ERROR" && step.value === "code") return e.fieldErrors().code ?? e.message;
  return null;
});

async function submit() {
  if (!username.value.trim() || !password.value) return;
  busy.value = true;
  error.value = null;
  restarted.value = null;
  if (ssoCode.value) void router.replace({ query: { ...route.query, ssoError: undefined } });
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
      <div v-if="ssoError" class="alert alert-error" role="alert" data-testid="sso-error">
        <strong>Single sign-on did not work.</strong>
        <div>{{ ssoError }}</div>
      </div>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" title="Could not sign in" />
      <template v-if="oidc.length > 0">
        <nav class="sso" aria-label="Single sign-on">
          <!-- Real links: the API redirects the browser to the provider and back. -->
          <a v-for="p in oidc" :key="p.id" class="btn block" :href="startHref(p.startUrl)">Sign in with {{ p.name }}</a>
        </nav>
        <div class="sso-divider" role="separator"><span>or with a username and password</span></div>
      </template>
      <div class="field">
        <label for="login-username">Username</label>
        <input id="login-username" v-model="username" v-autofocus type="text" autocomplete="username" required />
      </div>
      <div class="field">
        <label for="login-password">Password</label>
        <input id="login-password" v-model="password" v-autofocus="!!username" type="password" autocomplete="current-password" required />
      </div>
      <button type="submit" class="btn btn-primary block" :disabled="busy">{{ busy ? "Signing in…" : "Sign in" }}</button>
      <p v-if="directory" class="hint" data-testid="directory-hint">
        You can also sign in with your directory account (LDAP / Active Directory): use your usual network username and password.
      </p>
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
.sso {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
}
.sso-divider {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  font-size: var(--fs-xs);
  color: var(--c-text-muted);
}
.sso-divider::before,
.sso-divider::after {
  content: "";
  flex: 1;
  border-top: 1px solid var(--c-border);
}
.code-input {
  font-size: var(--fs-lg);
  letter-spacing: 0.2em;
}
</style>
