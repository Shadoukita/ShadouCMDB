<script setup lang="ts">
import { RouterLink } from "vue-router";
import { t } from "../i18n";
import { useSessionStore } from "../stores/session";
import EmptyState from "./EmptyState.vue";

/**
 * Shown wherever CIs would be (dashboard, inventory, new CI) while the data model
 * has no classes but the built-in ones (dataModelEmpty): a fresh install.
 * Administrators are sent to the starter template or the class editor; everyone
 * else is told whom to ask.
 */
const session = useSessionStore();
</script>

<template>
  <EmptyState :title="t('dataModel.empty.title')">
    <template v-if="session.can('datamodel.manage')">{{ t("dataModel.empty.admin") }}</template>
    <template v-else>{{ t("dataModel.empty.user") }}</template>
    <template v-if="session.can('datamodel.manage')" #actions>
      <RouterLink class="btn btn-primary" to="/admin/templates">{{ t("dataModel.empty.installTemplate") }}</RouterLink>
      <RouterLink class="btn" to="/admin/classes/new">+ {{ t("dataModel.empty.createClass") }}</RouterLink>
    </template>
  </EmptyState>
</template>
