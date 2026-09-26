<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { ciCountQuery, useCiClasses } from "../api/queries";
import { useSessionStore } from "../stores/session";
import NavLink from "./NavLink.vue";

/**
 * "Browse by class" is built from the API's class list, so new classes appear automatically. It lists only
 * the classes the user may view: the API filters the others out of every list, so their count would be a false 0.
 */
const classes = useCiClasses();
const session = useSessionStore();
const concrete = computed(() =>
  (classes.data.value ?? []).filter((c) => c.isActive && !c.isAbstract && session.canOnClass(c.id, "view")),
);
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
      <span>{{ c.name }}</span>
      <span class="muted">{{ counts[i]?.data ?? "" }}</span>
    </NavLink>
  </template>
</template>
