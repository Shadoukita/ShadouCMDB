<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { ciCountQuery, useCiClasses } from "../api/queries";
import NavLink from "./NavLink.vue";

/** "Browse by class" is built from the API's class list, so new classes appear automatically. */
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
      <span>{{ c.name }}</span>
      <span class="muted">{{ counts[i]?.data ?? "" }}</span>
    </NavLink>
  </template>
</template>
