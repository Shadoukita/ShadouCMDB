<script setup lang="ts">
import { computed, ref, watch, watchEffect } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import BrandMark from "./components/BrandMark.vue";
import ErrorAlert from "./components/ErrorAlert.vue";
import GlobalSearch from "./components/GlobalSearch.vue";
import LoadingState from "./components/LoadingState.vue";
import MainNav from "./components/MainNav.vue";
import NotificationsBell from "./components/NotificationsBell.vue";
import ReauthenticateDialog from "./components/ReauthenticateDialog.vue";
import ToastHost from "./components/ToastHost.vue";
import UserMenu from "./components/UserMenu.vue";
import { t } from "./i18n";
import { useMediaQuery } from "./lib/composables";
import { applyBranding, useBrandingStore } from "./stores/branding";
import { applyDensity, useDensityStore } from "./stores/density";
import { useSessionStore } from "./stores/session";
import Icon from "./components/Icon.vue";

const route = useRoute();
const session = useSessionStore();
const branding = useBrandingStore();
// Theme, brand colours and favicon follow the saved branding (or the editor's live preview).
watchEffect(() => applyBranding(branding.effective, branding.theme));
const density = useDensityStore();
watchEffect(() => applyDensity(density.density));

// Below 820 px the sidebar becomes a drawer behind a toggle in the brand cell (breakpoint also in app.css).
const narrow = useMediaQuery("(max-width: 820px)");
const navOpen = ref(false);
watch([() => route.fullPath, narrow], () => (navOpen.value = false));
// Desktop: the nav collapses to a 56 px icon rail; the choice is remembered in this browser.
const COLLAPSED_KEY = "shadoucmdb.nav.collapsed";
const collapsedPref = ref(readCollapsed());
const collapsed = computed(() => collapsedPref.value && !narrow.value);
function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}
function toggleCollapsed() {
  collapsedPref.value = !collapsedPref.value;
  try {
    if (collapsedPref.value) localStorage.setItem(COLLAPSED_KEY, "1");
    else localStorage.removeItem(COLLAPSED_KEY);
  } catch {
    // Storage disabled: the choice lasts for this page load.
  }
}

function onShellKey(e: KeyboardEvent) {
  if (e.key === "Escape" && navOpen.value) {
    navOpen.value = false;
    document.getElementById("nav-toggle")?.focus();
  }
}

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
  <div
    v-else-if="session.status === 'signedIn'"
    class="shell"
    :class="{ 'nav-open': narrow && navOpen, 'nav-collapsed': collapsed }"
    @keydown="onShellKey"
  >
    <div class="shell-brand">
      <button
        v-if="narrow"
        id="nav-toggle"
        type="button"
        class="nav-toggle"
        aria-controls="shell-nav"
        :aria-expanded="navOpen"
        :aria-label="navOpen ? t('shell.nav.close') : t('shell.nav.open')"
        @click="navOpen = !navOpen"
      >
        <Icon name="menu" :size="20" />
      </button>
      <RouterLink to="/" :aria-label="t('shell.home', { app: branding.effective.appName })"><BrandMark /></RouterLink>
    </div>
    <header class="shell-header">
      <GlobalSearch />
      <div class="shell-actions">
        <NotificationsBell />
        <RouterLink v-if="session.canOnAnyClass('create')" class="btn btn-primary new-ci" to="/cis/new" :title="t('shell.newCi')">
          <Icon name="plus" /><span class="btn-label">{{ t("shell.newCi") }}</span>
        </RouterLink>
        <UserMenu v-if="narrow" />
      </div>
    </header>
    <nav id="shell-nav" class="shell-nav" :aria-label="t('shell.mainNav')">
      <div class="nav-body">
        <MainNav :collapsed="collapsed" />
      </div>
      <div v-if="!narrow" class="nav-footer">
        <button
          type="button"
          class="nav-collapse"
          aria-controls="shell-nav"
          :aria-expanded="!collapsed"
          :aria-label="collapsed ? t('shell.nav.expand') : t('shell.nav.collapse')"
          :title="collapsed ? t('shell.nav.expand') : t('shell.nav.collapse')"
          @click="toggleCollapsed"
        >
          <Icon :name="collapsed ? 'panel-left-open' : 'panel-left-close'" :size="collapsed ? 20 : 16" />
          <span v-if="!collapsed" class="nav-label" aria-hidden="true">{{ t("shell.nav.collapse") }}</span>
        </button>
        <UserMenu placement="rail" />
      </div>
    </nav>
    <div v-if="narrow && navOpen" class="nav-scrim" aria-hidden="true" @click="navOpen = false"></div>
    <main id="main" class="shell-main">
      <RouterView />
    </main>
    <ToastHost />
    <ReauthenticateDialog />
  </div>
  <LoadingState v-else :label="t('shell.starting')" />
</template>
