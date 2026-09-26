<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ciCountQuery, useCiClasses } from "../api/queries";
import { useSessionStore } from "../stores/session";
import ClassBadge from "./ClassBadge.vue";
import NavLink from "./NavLink.vue";

/** "Browse by class" is built from the API's class list (in the administrator's order), so new classes appear automatically. */
const session = useSessionStore();
const classes = useCiClasses();
const concrete = computed(() => (classes.data.value ?? []).filter((c) => c.isActive && !c.isAbstract));
const counts = useQueries({ queries: computed(() => concrete.value.map((c) => ciCountQuery({ classId: c.id }))) });
</script>

<template>
  <h2 v-if="classes.isError.value">Classes unavailable</h2>
  <template v-else>
    <h2>Browse by class</h2>
    <NavLink
      v-for="(c, i) in concrete"
      :key="c.id"
      :to="`/cis?classId=${c.id}`"
      :active="(r) => r.path === '/cis' && r.query.classId === c.id"
    >
      <ClassBadge :icon="c.icon" :color="c.color" :name="c.name" />
      <span class="muted">{{ counts[i]?.data ?? "" }}</span>
    </NavLink>
    <p v-if="classes.data.value && concrete.length === 0" class="nav-note">
      No classes yet.
      <RouterLink v-if="session.can('datamodel.manage')" to="/admin/templates">Set up the data model</RouterLink>
    </p>
  </template>
</template>
