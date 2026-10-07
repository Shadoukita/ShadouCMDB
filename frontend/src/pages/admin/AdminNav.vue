<script setup lang="ts">
import { computed } from "vue";
import NavLink from "../../components/NavLink.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";
import { groupedSections } from "./sections";

/**
 * The Administration sections the user may open, under their group labels (design §3, PR 8; audit A1, A2).
 * It sits in the expanded rail under "Administration", so it follows the rail into the drawer under 820 px;
 * AdminLayout shows it beside the page only while the rail is collapsed to icons or does not list
 * Administration. Group labels are not headings, so the page h1 stays the first heading of the content.
 */
defineProps<{ placement: "rail" | "page" }>();
const session = useSessionStore();
const groups = computed(() => groupedSections(session.adminAccess));
</script>

<template>
  <nav :class="['admin-nav', `admin-nav-${placement}`]" :aria-label="t('common.administration')">
    <div v-for="(g, i) in groups" :key="g.group" class="admin-nav-group" role="group" :aria-labelledby="`admin-nav-${placement}-${i}`">
      <span :id="`admin-nav-${placement}-${i}`" class="admin-nav-label">{{ g.group }}</span>
      <NavLink v-for="s in g.sections" :key="s.key" :to="s.to" :active="(r) => r.path === s.to || r.path.startsWith(`${s.to}/`)">
        {{ s.label }}
      </NavLink>
    </div>
  </nav>
</template>
