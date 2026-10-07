<script setup lang="ts">
import { computed } from "vue";
import { RouterView, useRoute } from "vue-router";
import EmptyState from "../../components/EmptyState.vue";
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
const allowed = computed(() => sectionAllowed({ permissions: required.value, administratorOnly: administratorOnly.value }, session.adminAccess));
</script>

<template>
  <EmptyState v-if="sections.length === 0" icon="lock" :title="t('admin.noAccess.title')">{{ t("admin.noAccess.body") }}</EmptyState>
  <div v-else :class="['admin', { 'admin-with-nav': !adminNavInRail }]">
    <AdminNav v-if="!adminNavInRail" placement="page" />
    <div class="admin-body">
      <RouterView v-if="allowed" />
      <EmptyState v-else-if="administratorOnly" icon="lock" :title="t('admin.denied.title')">
        {{ t("admin.denied.administratorOnly") }}
        <template v-if="route.path.startsWith('/admin/identity-providers')">{{ t("admin.denied.identityProviders") }}</template>
      </EmptyState>
      <EmptyState v-else icon="lock" :title="t('admin.denied.title')">
        {{ t("admin.denied.needs", { n: required.length }) }}
        <span class="denied-permissions"><code v-for="p in required" :key="p">{{ p }}</code></span>
      </EmptyState>
    </div>
  </div>
</template>
