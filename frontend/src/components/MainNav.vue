<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, ref, watch, watchEffect } from "vue";
import { RouterLink, useRoute, type RouteLocationNormalizedLoaded } from "vue-router";
import { useAreas } from "../api/datamodel";
import { ciCountQuery, useCiClasses } from "../api/queries";
import { useServiceSettings } from "../api/services";
import { t } from "../i18n";
import { dataModelEmpty } from "../lib/dataModel";
import type { UiPage } from "../api/uiSettings";
import { useAppSettings, useNavPreviewStore } from "../lib/appSettings";
import { viewableClasses } from "../lib/permissions";
import { useImportAccess } from "../lib/useImportAccess";
import { buildNav, type NavLinkItem } from "../lib/uiSettings";
import AdminNav from "../pages/admin/AdminNav.vue";
import { adminNavInRail } from "../pages/admin/adminNavHost";
import { visibleSections } from "../pages/admin/sections";
import { useSessionStore } from "../stores/session";
import ClassBadge from "./ClassBadge.vue";
import Icon from "./Icon.vue";
import NavLink from "./NavLink.vue";
import type { IconName } from "../icons/lucide";

/** Collapsed to the 56 px icon rail (desktop only): pages show as icons with a tooltip, classes are hidden. */
const props = defineProps<{ collapsed?: boolean }>();

/**
 * The main menu. Its order, names, sections and hidden entries come from
 * Administration › Customization › Navigation; pages and classes the settings
 * do not mention follow in the built-in order, so new classes appear by
 * themselves. Classes that no section claims sit under their area's tab
 * (Administration › Areas), which folds open and shut; the folded tabs are
 * remembered in this browser. Pages the user may not open are never shown, nor
 * classes they may not view: the API filters those out of every list, so their
 * count would be a false 0.
 */
const session = useSessionStore();
const classes = useCiClasses();
const areas = useAreas();
const { doc } = useAppSettings();
const preview = useNavPreviewStore();

const importAccess = useImportAccess();
/** Business services sit under Inventory for users who may view them (GET /settings/business-services). */
const services = useServiceSettings();
const showServices = computed(() => !!services.data.value?.canView);
const hasAdmin = computed(() => visibleSections(session.adminAccess).length > 0);
function showPage(p: UiPage): boolean {
  if (p === "audit_log") return session.can("audit.view");
  if (p === "administration") return hasAdmin.value;
  return true;
}
const navClasses = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")));
const groups = computed(() =>
  buildNav(preview.entries ?? doc.value.navigation.entries, navClasses.value, showPage, areas.data.value ?? []),
);

const FOLDED_KEY = "shadoucmdb.nav.folded";
function readFolded(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(FOLDED_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}
/** Area keys whose tab is folded shut. */
const folded = ref(new Set(readFolded()));
watch(folded, (f) => localStorage.setItem(FOLDED_KEY, JSON.stringify([...f])), { deep: true });
function toggle(key: string) {
  const next = new Set(folded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  folded.value = next;
}
/** Each built-in page's rail icon. */
const PAGE_ICONS: Record<UiPage, IconName> = {
  dashboard: "layout-dashboard",
  inventory: "list",
  search: "search",
  audit_log: "scroll-text",
  administration: "settings",
};

const items = computed(() => groups.value.flatMap((g) => g.items));
/**
 * In Administration the expanded rail lists its sections under the Administration entry (design §3, PR 8),
 * so the page needs no second navigation column and the sections follow the rail into the drawer.
 */
const route = useRoute();
const adminSub = computed(
  () => !props.collapsed && route.path.startsWith("/admin") && hasAdmin.value && items.value.some((i) => i.page === "administration"),
);
watchEffect(() => (adminNavInRail.value = adminSub.value));
onBeforeUnmount(() => (adminNavInRail.value = false));
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
    <template v-if="!collapsed">
      <h2 v-if="g.area" class="nav-area">
        <button
          type="button"
          :aria-expanded="!folded.has(g.area.key)"
          :aria-controls="`nav-${g.id}`"
          :title="t(folded.has(g.area.key) ? 'nav.area.show' : 'nav.area.hide', { area: g.area.name })"
          @click="toggle(g.area.key)"
        >
          <Icon class="nav-fold" :size="14" :name="folded.has(g.area.key) ? 'chevron-right' : 'chevron-down'" />
          <ClassBadge :icon="g.area.icon" :color="g.area.color" plain :name="g.heading ?? ''" />
        </button>
      </h2>
      <h2 v-else-if="g.heading">{{ g.heading }}</h2>
      <div v-if="g.area" v-show="!folded.has(g.area.key)" :id="`nav-${g.id}`" class="nav-area-items">
        <NavLink v-for="item in g.items" :key="item.id" :to="item.to" :active="active(item)">
          <ClassBadge v-if="item.cls" :icon="item.cls.icon" :color="item.cls.color" plain :name="item.label" />
          <span v-if="item.cls" class="nav-count">{{ countFor(item) ?? "" }}</span>
        </NavLink>
      </div>
    </template>
    <template v-for="item in g.area ? [] : g.items" :key="item.id">
      <template v-if="item.page">
        <NavLink :to="item.to" :active="active(item)" :title="collapsed ? item.label : undefined">
          <Icon :name="PAGE_ICONS[item.page]" :size="collapsed ? 20 : 16" />
          <span class="nav-label">{{ item.label }}</span>
        </NavLink>
        <AdminNav v-if="item.page === 'administration' && adminSub" placement="rail" :rail-items="items.length" />
        <!-- Bulk import sits under Inventory, only while it is switched on and the user holds cis.import. -->
        <NavLink
          v-if="item.page === 'inventory' && importAccess.available.value"
          to="/imports"
          :active="(r) => r.path.startsWith('/imports')"
          :title="collapsed ? t('nav.bulkImport') : undefined"
        >
          <Icon name="upload" :size="collapsed ? 20 : 16" />
          <span class="nav-label">{{ t("nav.bulkImport") }}</span>
        </NavLink>
        <NavLink
          v-if="item.page === 'inventory' && showServices"
          to="/services"
          :active="(r) => r.path.startsWith('/services')"
          :title="collapsed ? t('services.nav') : undefined"
        >
          <Icon name="layers" :size="collapsed ? 20 : 16" />
          <span class="nav-label">{{ t("services.nav") }}</span>
        </NavLink>
        <NavLink
          v-if="item.page === 'inventory'"
          to="/workflows"
          :active="(r) => r.path.startsWith('/workflows')"
          :title="collapsed ? t('workflows.nav') : undefined"
        >
          <Icon name="circle-check" :size="collapsed ? 20 : 16" />
          <span class="nav-label">{{ t("workflows.nav") }}</span>
        </NavLink>
      </template>
      <NavLink v-else-if="item.cls && !collapsed" :to="item.to" :active="active(item)">
        <ClassBadge :icon="item.cls.icon" :color="item.cls.color" plain :name="item.label" />
        <span class="nav-count">{{ countFor(item) ?? "" }}</span>
      </NavLink>
    </template>
  </template>
  <template v-if="!collapsed">
    <p v-if="classes.isError.value" class="nav-note">{{ t("nav.classesUnavailable") }}</p>
    <!-- The built-in classes (Business service) do not count: until a class of its own exists, the data model is empty. -->
    <p v-else-if="classes.data.value && dataModelEmpty(classes.data.value)" class="nav-note">
      {{ t("nav.noClasses") }}
      <RouterLink v-if="session.can('datamodel.manage')" to="/admin/templates">{{ t("nav.setUpDataModel") }}</RouterLink>
    </p>
  </template>
</template>
