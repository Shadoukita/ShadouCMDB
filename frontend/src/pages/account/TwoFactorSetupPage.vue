<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import SignedInAs from "../../components/SignedInAs.vue";
import { t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { safeRedirect } from "../../lib/signIn";
import { useSessionStore } from "../../stores/session";
import TwoFactorSettings from "./TwoFactorSettings.vue";

/**
 * Forced enrolment: a permission profile the user holds requires two-factor
 * authentication and they have not set it up. Until they do, the API answers
 * every other route with 403 MFA_ENROLMENT_REQUIRED, so this screen stands alone
 * (no navigation, no search) and offers only set-up and sign-out.
 *
 * A session opened with the password alone stays limited even when the account
 * already has an authenticator (set up in another browser): it has to sign in
 * again with a code, so then this screen says that instead of offering a set-up
 * the API would refuse (409).
 */
useDocumentTitle(() => t("account.enrol.documentTitle"));
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const signOutError = ref<unknown>(null);
/** Both from the same /auth/me answer: right after a set-up here the session is no longer limited, so the recovery codes stay on screen. */
const alreadySetUp = computed(() => session.enrolmentRequired && !!session.session?.mfa.totpEnabled);

/** Back to the sign-in screen, which returns to where the user was going. */
async function signOut() {
  signOutError.value = null;
  try {
    await session.logout();
    const redirect = safeRedirect(route.query.redirect);
    await router.replace({ path: "/login", query: redirect === "/" ? {} : { redirect } });
  } catch (e) {
    signOutError.value = e;
  }
}

function enrolled() {
  void router.replace(safeRedirect(route.query.redirect));
}
</script>

<template>
  <main class="bare">
    <div class="bare-card wide" data-testid="two-factor-setup">
      <div class="bare-brand"><BrandMark /></div>
      <h1>{{ alreadySetUp ? t("account.enrol.againTitle") : t("account.enrol.title") }}</h1>
      <p class="lead">{{ alreadySetUp ? t("account.enrol.alreadySetUp") : t("account.mfa.forced") }}</p>
      <ErrorAlert v-if="signOutError" :error="signOutError" :title="t('account.enrol.signOutFailed')" />
      <template v-if="alreadySetUp">
        <p class="hint">{{ t("account.enrol.noApp") }}</p>
        <button type="button" class="btn btn-primary block" @click="signOut">{{ t("account.enrol.signOutAndIn") }}</button>
      </template>
      <TwoFactorSettings v-else forced @enrolled="enrolled" />
      <div class="bare-foot">
        <SignedInAs v-if="session.user" :display-name="session.user.displayName" :username="session.user.username" data-testid="two-factor-setup-identity" />
        <p v-if="!alreadySetUp">
          <button type="button" class="btn-link" @click="signOut">{{ t("account.enrol.signOutLater") }}</button>
        </p>
      </div>
    </div>
  </main>
</template>
