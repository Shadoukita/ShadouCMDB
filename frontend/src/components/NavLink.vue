<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute, type RouteLocationNormalizedLoaded } from "vue-router";

/** Sidebar link whose "active" rule is explicit (query-aware), unlike RouterLink's path matching. */
const props = defineProps<{ to: string; active: (route: RouteLocationNormalizedLoaded) => boolean }>();
const route = useRoute();
const isActive = computed(() => props.active(route));
</script>

<template>
  <RouterLink v-slot="{ href, navigate }" :to="to" custom>
    <a :href="href" :class="{ active: isActive }" :aria-current="isActive ? 'page' : undefined" @click="navigate">
      <slot />
    </a>
  </RouterLink>
</template>
