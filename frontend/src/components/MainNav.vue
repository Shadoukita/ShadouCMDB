<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink, type RouteLocationNormalizedLoaded } from "vue-router";
import { ciCountQuery, useCiClasses } from "../api/queries";
import type { UiPage } from "../api/uiSettings";
import { useAppSettings, useNavPreviewStore } from "../lib/appSettings";
import { viewableClasses } from "../lib/permissions";
import { buildNav, type NavLinkItem } from "../lib/uiSettings";
import { visibleSections } from "../pages/admin/sections";
import { useSessionStore } from "../stores/session";
import ClassBadge from "./ClassBadge.vue";
import NavLink from "./NavLink.vue";

/**
 * The main menu. Its order, names, sections and hidden entries come from
 * Administration › Customization › Navigation; pages and classes the settings
 * do not mention follow in the built-in order, so new classes appear by
 * themselves. Pages the user may not open are never shown, nor classes they may
 * not view: the API filters those out of every list, so their count would be a false 0.
 */
const session = useSessionStore();
const classes = useCiClasses();
const { doc } = useAppSettings();
const preview = useNavPreviewStore();

const hasAdmin = computed(() => visibleSections(session.can).length > 0);
function showPage(p: UiPage): boolean {
  if (p === "audit_log") return session.can("audit.view");
  if (p === "administration") return hasAdmin.value;
  return true;
}
const navClasses = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")));
const groups = computed(() => buildNav(preview.entries ?? doc.value.navigation.entries, navClasses.value, showPage));
const items = computed(() => groups.value.flatMap((g) => g.items));
const auditShown = computed(() => items.value.some((i) => i.page === "audit_log"));

const classItems = computed(() => items.value.filter((i) => i.cls && !i.cls.isAbstract));
const counts = useQueries({ queries: computed(() => classItems.value.map((i) => ciCountQuery({ classId: i.cls!.id }))) });
const countFor = (item: NavLinkItem) => {
  const i = classItems.value.indexOf(item);
  return i >= 0 ? counts.value[i]?.data : undefined;
};

function active(item: NavLinkItem): (r: RouteLocationNormalizedLoaded) => boolean {
  if (item.cls) return (r) => r.path === "/cis" && r.query.classId === item.cls!.id;
  switch (item.page) {
    case "dashboard":
      return (r) => r.path === "/";
    case "inventory":
      return (r) => r.path === "/cis" && !r.query.classId;
    case "search":
      return (r) => r.path === "/search";
    case "audit_log":
      return (r) => r.path.startsWith("/admin/audit");
    default:
      return (r) => r.path.startsWith("/admin") && !(auditShown.value && r.path.startsWith("/admin/audit"));
  }
}
</script>

<template>
  <template v-for="g in groups" :key="g.id">
    <h2 v-if="g.heading">{{ g.heading }}</h2>
    <NavLink v-for="item in g.items" :key="item.id" :to="item.to" :active="active(item)">
      <ClassBadge v-if="item.cls" :icon="item.cls.icon" :color="item.cls.color" :name="item.label" />
      <template v-else>{{ item.label }}</template>
      <span v-if="item.cls" class="muted">{{ countFor(item) ?? "" }}</span>
    </NavLink>
  </template>
  <p v-if="classes.isError.value" class="nav-note">Classes unavailable</p>
  <p v-else-if="classes.data.value && classes.data.value.length === 0" class="nav-note">
    No classes yet.
    <RouterLink v-if="session.can('datamodel.manage')" to="/admin/templates">Set up the data model</RouterLink>
  </p>
</template>
