<script setup lang="ts">
import { watchEffect } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import BrandMark from "./components/BrandMark.vue";
import ErrorAlert from "./components/ErrorAlert.vue";
import GlobalSearch from "./components/GlobalSearch.vue";
import LoadingState from "./components/LoadingState.vue";
import MainNav from "./components/MainNav.vue";
import UserMenu from "./components/UserMenu.vue";
import { applyBranding, useBrandingStore } from "./stores/branding";
import { useSessionStore } from "./stores/session";

const route = useRoute();
const session = useSessionStore();
const branding = useBrandingStore();
// Theme, brand colours and favicon follow the saved branding (or the editor's live preview).
watchEffect(() => applyBranding(branding.effective, branding.theme));

function retry() {
  window.location.reload();
}
</script>

<template>
  <div v-if="session.bootError" class="bare">
    <div class="bare-card wide">
      <h1>{{ branding.effective.appName }}</h1>
      <ErrorAlert :error="session.bootError" :on-retry="retry" />
    </div>
  </div>
  <RouterView v-else-if="route.meta.public || route.meta.bare" />
  <div v-else-if="session.status === 'signedIn'" class="shell">
    <div class="shell-brand">
      <RouterLink to="/" :aria-label="`${branding.effective.appName} home`"><BrandMark /></RouterLink>
    </div>
    <header class="shell-header">
      <GlobalSearch />
      <div class="actions" style="margin-left: auto">
        <RouterLink v-if="session.canOnAnyClass('create')" class="btn btn-primary" to="/cis/new">+ New CI</RouterLink>
        <UserMenu />
      </div>
    </header>
    <nav class="shell-nav" aria-label="Main">
      <MainNav />
    </nav>
    <main id="main" class="shell-main">
      <RouterView />
    </main>
  </div>
  <LoadingState v-else label="Starting…" />
</template>
