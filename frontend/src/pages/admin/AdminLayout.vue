<script setup lang="ts">
import { computed } from "vue";
import { RouterView, useRoute } from "vue-router";
import PermissionDenied from "../../components/PermissionDenied.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";
import AdminNav from "./AdminNav.vue";
import { adminNavInRail } from "./adminNavHost";
import { sectionAllowed, visibleSections } from "./sections";

/**
 * The Administration area. Its sub-navigation (only the sections the user holds a permission for)
 * sits in the expanded rail; beside the page only while the rail does not show it (adminNavHost.ts). Opening a section without the permission shows a
 * designed permission-denied state (the API would answer 403 anyway).
 */
const route = useRoute();
const session = useSessionStore();
const sections = computed(() => visibleSections(session.adminAccess));
const required = computed(() => route.meta.permissions ?? []);
const administratorOnly = computed(() => !!route.meta.administratorOnly);
const crumbs = computed(() => [{ label: t("nav.page.administration") }]);
const allowed = computed(() => sectionAllowed({ permissions: required.value, administratorOnly: administratorOnly.value }, session.adminAccess));
</script>

<template>
  <PermissionDenied v-if="sections.length === 0" :crumbs="crumbs" :panel-title="t('admin.noAccess.title')">{{ t("admin.noAccess.body") }}</PermissionDenied>
  <div v-else :class="['admin', { 'admin-with-nav': !adminNavInRail }]">
    <AdminNav v-if="!adminNavInRail" placement="page" />
    <div class="admin-body">
      <RouterView v-if="allowed" />
      <PermissionDenied v-else-if="administratorOnly" :crumbs="crumbs" :requirement="t('denied.administrator')" :panel-title="t('admin.denied.panelTitle')">
        {{ t("admin.denied.administratorOnly") }}
        <template v-if="route.path.startsWith('/admin/identity-providers')">{{ t("admin.denied.identityProviders") }}</template>
      </PermissionDenied>
      <PermissionDenied v-else :crumbs="crumbs" :permissions="required" :panel-title="t('admin.denied.panelTitle')">
        {{ t("admin.denied.body") }}
      </PermissionDenied>
    </div>
  </div>
</template>
