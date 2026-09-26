<script setup lang="ts">
import { RouterLink, RouterView } from "vue-router";
import ClassNav from "./components/ClassNav.vue";
import GlobalSearch from "./components/GlobalSearch.vue";
import NavLink from "./components/NavLink.vue";
</script>

<template>
  <div class="shell">
    <div class="shell-brand">
      <RouterLink to="/">ShadouCMDB</RouterLink>
    </div>
    <header class="shell-header">
      <GlobalSearch />
      <div class="actions" style="margin-left: auto">
        <RouterLink class="btn btn-primary" to="/cis/new">+ New CI</RouterLink>
      </div>
    </header>
    <nav class="shell-nav" aria-label="Main">
      <NavLink to="/" :active="(r) => r.path === '/'">Dashboard</NavLink>
      <NavLink to="/cis" :active="(r) => r.path === '/cis' && !r.query.classId">All configuration items</NavLink>
      <ClassNav />
    </nav>
    <main id="main" class="shell-main">
      <RouterView />
    </main>
  </div>
</template>
