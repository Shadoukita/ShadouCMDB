<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { oidcStartHref, useSignInOptions } from "../../api/identityProviders";
import { normaliseCode } from "../../api/mfa";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t, tAround } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { safeRedirect, ssoErrorMessage } from "../../lib/signIn";
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
useDocumentTitle(() => t("auth.signIn.title"));
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

const ssoCode = computed(() => (typeof route.query.ssoError === "string" ? route.query.ssoError : null));
const ssoError = computed(() => ssoErrorMessage(ssoCode.value));
const startHref = (startUrl: string) => oidcStartHref(startUrl, redirect.value);
const lostAdmin = computed(() => tAround("auth.signIn.lostAdmin", "command"));
const signingInAs = computed(() => tAround("auth.mfa.signingInAs", "username"));

const apiError = computed(() => (error.value instanceof ApiError ? error.value : null));
/** Wrong credentials and lockouts are expected answers, phrased for the operator; anything else shows in full. */
const known = computed(() => {
  const e = apiError.value;
  if (e?.code === "UNAUTHENTICATED") {
    if (step.value === "code") return useRecovery.value ? t("auth.signIn.wrongRecoveryCode") : t("auth.signIn.wrongCode");
    return t("auth.signIn.wrongPassword");
  }
  if (e?.code === "RATE_LIMITED") return e.message || t("auth.signIn.rateLimited");
  if (e?.code === "IDENTITY_PROVIDER_UNAVAILABLE") {
    return t("auth.signIn.directoryUnavailable");
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
      <h1 id="login-title">{{ t("auth.signIn.title") }}</h1>
      <p class="lead">{{ t("auth.signIn.lead") }}</p>
      <div v-if="session.expired" class="alert alert-warn" role="status">
        {{ t("auth.signIn.sessionEnded") }}
      </div>
      <div v-if="restarted" class="alert alert-warn" role="status">{{ restarted }}</div>
      <div v-if="ssoError" class="alert alert-error" role="alert" data-testid="sso-error">
        <strong>{{ t("auth.signIn.ssoFailedTitle") }}</strong>
        <div>{{ ssoError }}</div>
      </div>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('auth.signIn.failed')" />
      <template v-if="oidc.length > 0">
        <nav class="sso" :aria-label="t('auth.signIn.ssoNav')">
          <!-- Real links: the API redirects the browser to the provider and back. -->
          <a v-for="p in oidc" :key="p.id" class="btn block" :href="startHref(p.startUrl)">{{ t("auth.signIn.withProvider", { name: p.name }) }}</a>
        </nav>
        <div class="sso-divider" role="separator"><span>{{ t("auth.signIn.orPassword") }}</span></div>
      </template>
      <div class="field">
        <label for="login-username">{{ t("auth.signIn.username") }}</label>
        <input id="login-username" v-model="username" v-autofocus type="text" autocomplete="username" required />
      </div>
      <div class="field">
        <label for="login-password">{{ t("auth.signIn.password") }}</label>
        <input id="login-password" v-model="password" v-autofocus="!!username" type="password" autocomplete="current-password" required />
      </div>
      <button type="submit" class="btn btn-primary block" :disabled="busy">{{ busy ? t("auth.signIn.submitting") : t("auth.signIn.submit") }}</button>
      <div class="bare-foot">
        <p v-if="directory" class="hint" data-testid="directory-hint">
          {{ t("auth.signIn.directoryHint") }}
        </p>
        <p class="hint">{{ lostAdmin[0] }}<code>shadoucmdb create-admin</code>{{ lostAdmin[1] }}</p>
      </div>
    </form>

    <form v-else class="bare-card" aria-labelledby="mfa-title" novalidate @submit.prevent="submitCode">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="mfa-title">{{ t("auth.mfa.title") }}</h1>
      <p class="lead">
        {{ signingInAs[0] }}<strong>{{ username }}</strong>{{ signingInAs[1] }}
        {{ useRecovery ? t("auth.mfa.recoveryIntro") : t("auth.mfa.totpIntro") }}
      </p>
      <div v-if="known" class="alert alert-error" role="alert">{{ known }}</div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('auth.signIn.failed')" />
      <div v-if="!useRecovery" class="field">
        <label for="login-code">{{ t("auth.mfa.code") }}</label>
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
        <label for="login-recovery">{{ t("auth.mfa.recoveryCode") }}</label>
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
      <button type="submit" class="btn btn-primary block" :disabled="busy || !code.trim()">{{ busy ? t("auth.mfa.verifying") : t("auth.mfa.verify") }}</button>
      <div class="bare-foot">
        <p class="mfa-links">
          <button type="button" class="btn-link" @click="toggleRecovery">
            {{ useRecovery ? t("auth.mfa.useApp") : t("auth.mfa.useRecovery") }}
          </button>
          <button type="button" class="btn-link" @click="restart()">{{ t("auth.mfa.someoneElse") }}</button>
        </p>
        <p class="hint">{{ t("auth.mfa.noDevice") }}</p>
      </div>
    </form>
  </main>
</template>

<style scoped>
.sso {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}
.sso-divider {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-sm);
  color: var(--c-text-secondary);
}
.sso-divider::before,
.sso-divider::after {
  content: "";
  flex: 1;
  border-top: 1px solid var(--c-border);
}
.code-input {
  height: auto;
  padding-block: var(--space-2);
  font-size: var(--fs-h1);
  letter-spacing: 0.3em;
  text-align: center;
}
.mfa-links {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-1);
}
</style>
