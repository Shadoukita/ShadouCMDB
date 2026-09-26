<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import ClassNav from "./components/ClassNav.vue";
import ErrorAlert from "./components/ErrorAlert.vue";
import GlobalSearch from "./components/GlobalSearch.vue";
import LoadingState from "./components/LoadingState.vue";
import NavLink from "./components/NavLink.vue";
import UserMenu from "./components/UserMenu.vue";
import { visibleSections } from "./pages/admin/sections";
import { useSessionStore } from "./stores/session";

const route = useRoute();
const session = useSessionStore();
const hasAdmin = computed(() => visibleSections(session.can).length > 0);

function retry() {
  window.location.reload();
}
</script>

<template>
  <div v-if="session.bootError" class="bare">
    <div class="bare-card wide">
      <h1>ShadouCMDB</h1>
      <ErrorAlert :error="session.bootError" :on-retry="retry" />
    </div>
  </div>
  <RouterView v-else-if="route.meta.public" />
  <div v-else-if="session.status === 'signedIn'" class="shell">
    <div class="shell-brand">
      <RouterLink to="/">ShadouCMDB</RouterLink>
    </div>
    <header class="shell-header">
      <GlobalSearch />
      <div class="actions" style="margin-left: auto">
        <RouterLink v-if="session.canOnAnyClass('create')" class="btn btn-primary" to="/cis/new">+ New CI</RouterLink>
        <UserMenu />
      </div>
    </header>
    <nav class="shell-nav" aria-label="Main">
      <NavLink to="/" :active="(r) => r.path === '/'">Dashboard</NavLink>
      <NavLink to="/cis" :active="(r) => r.path === '/cis' && !r.query.classId">All configuration items</NavLink>
      <ClassNav />
      <template v-if="hasAdmin">
        <h2>System</h2>
        <NavLink to="/admin" :active="(r) => r.path.startsWith('/admin')">Administration</NavLink>
      </template>
    </nav>
    <main id="main" class="shell-main">
      <RouterView />
    </main>
  </div>
  <LoadingState v-else label="Starting…" />
</template>
