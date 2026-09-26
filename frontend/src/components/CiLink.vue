<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { extendTrail, type TrailStep } from "../lib/trail";

/** Link to a CI detail page. Pass `from` + `trail` to carry the walk trail into the breadcrumb. */
const props = withDefaults(defineProps<{ id: string; from?: TrailStep; trail?: TrailStep[] }>(), { from: undefined, trail: () => [] });
const to = computed(() => ({
  path: `/cis/${props.id}`,
  state: props.from ? { trail: extendTrail(props.trail, props.from, props.id) } : undefined,
}));
</script>

<template>
  <RouterLink :to="to"><slot /></RouterLink>
</template>
