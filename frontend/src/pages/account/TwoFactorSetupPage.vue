<script setup lang="ts">
import { ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { useDocumentTitle } from "../../lib/composables";
import { safeRedirect } from "../../router";
import { useSessionStore } from "../../stores/session";
import TwoFactorSettings from "./TwoFactorSettings.vue";

/**
 * Forced enrolment: a permission profile the user holds requires two-factor
 * authentication and they have not set it up. Until they do, the API answers
 * every other route with 403 MFA_ENROLMENT_REQUIRED, so this screen stands alone
 * (no navigation, no search) and offers only set-up and sign-out.
 */
useDocumentTitle("Set up two-factor authentication");
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const signOutError = ref<unknown>(null);

async function signOut() {
  signOutError.value = null;
  try {
    await session.logout();
    await router.replace("/login");
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
    <div class="enrol-page stack">
      <div class="bare-brand"><BrandMark /></div>
      <div class="page-header">
        <div class="title">
          <h1>Set up two-factor authentication</h1>
          <span v-if="session.user" class="muted">{{ session.user.displayName }} ({{ session.user.username }})</span>
        </div>
        <div class="actions">
          <button type="button" class="btn" @click="signOut">Sign out</button>
        </div>
      </div>
      <ErrorAlert v-if="signOutError" :error="signOutError" title="Sign-out failed" />
      <TwoFactorSettings forced @enrolled="enrolled" />
    </div>
  </main>
</template>

<style scoped>
.enrol-page {
  width: min(820px, 100%);
}
</style>
