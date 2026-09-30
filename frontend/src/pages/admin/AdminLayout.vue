<script setup lang="ts">
import { computed } from "vue";
import { RouterView, useRoute } from "vue-router";
import EmptyState from "../../components/EmptyState.vue";
import NavLink from "../../components/NavLink.vue";
import { useSessionStore } from "../../stores/session";
import { groupedSections, sectionAllowed, visibleSections } from "./sections";

/**
 * The Administration area: its own sub-navigation, showing only the sections the
 * user holds a permission for. Opening a section without the permission shows a
 * designed permission-denied state (the API would answer 403 anyway).
 */
const route = useRoute();
const session = useSessionStore();
const sections = computed(() => visibleSections(session.adminAccess));
const groups = computed(() => groupedSections(session.adminAccess));
const required = computed(() => route.meta.permissions ?? []);
const administratorOnly = computed(() => !!route.meta.administratorOnly);
const allowed = computed(() => sectionAllowed({ permissions: required.value, administratorOnly: administratorOnly.value }, session.adminAccess));
</script>

<template>
  <EmptyState v-if="sections.length === 0" title="You do not have access to Administration">
    Your permission profiles grant no administration rights. Ask an administrator if you need one.
  </EmptyState>
  <div v-else class="admin">
    <nav class="admin-nav" aria-label="Administration">
      <template v-for="g in groups" :key="g.group">
        <h2>{{ g.group }}</h2>
        <NavLink v-for="s in g.sections" :key="s.key" :to="s.to" :active="(r) => r.path === s.to || r.path.startsWith(`${s.to}/`)">
          {{ s.label }}
        </NavLink>
      </template>
    </nav>
    <div class="admin-body">
      <RouterView v-if="allowed" />
      <EmptyState v-else-if="administratorOnly" title="Permission denied">
        This screen is only for holders of the built-in <strong>Administrator</strong> permission profile.
        <template v-if="route.path.startsWith('/admin/identity-providers')">
          The <code>users.manage</code> permission alone is not enough: identity providers decide who may sign in and
          with which profiles.
        </template>
      </EmptyState>
      <EmptyState v-else title="Permission denied">
        This screen needs the
        <template v-for="(p, i) in required" :key="p"><template v-if="i > 0"> or </template><code>{{ p }}</code></template>
        permission, which none of your permission profiles grants.
      </EmptyState>
    </div>
  </div>
</template>
