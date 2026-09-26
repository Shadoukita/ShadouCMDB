<script setup lang="ts">
import { RouterLink } from "vue-router";
import { useSessionStore } from "../stores/session";
import EmptyState from "./EmptyState.vue";

/**
 * Shown wherever CIs would be (dashboard, inventory, new CI) while the data model
 * has no classes: a fresh install. Administrators are sent to the starter template
 * or the class editor; everyone else is told whom to ask.
 */
const session = useSessionStore();
</script>

<template>
  <EmptyState title="No CI classes are defined yet">
    <template v-if="session.can('datamodel.manage')">
      Before anyone can record a configuration item, the CMDB needs a data model: the classes of things you track
      (servers, applications, databases…), their attributes and the statuses a CI can have. Install the IT infrastructure
      starter to begin with a ready-made model, or build your own under Administration.
    </template>
    <template v-else>
      The CMDB has no data model yet, so there is nothing to record or browse. Ask an administrator to set it up under
      Administration › Data model.
    </template>
    <template v-if="session.can('datamodel.manage')" #actions>
      <RouterLink class="btn btn-primary" to="/admin/templates">Install a starter template</RouterLink>
      <RouterLink class="btn" to="/admin/classes/new">+ Create a class</RouterLink>
    </template>
  </EmptyState>
</template>
