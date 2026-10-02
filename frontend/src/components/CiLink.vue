<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { extendTrail, type TrailStep } from "../lib/trail";

/**
 * Link to a CI detail page. Pass `from` + `trail` to carry the walk trail into the breadcrumb.
 * `dir="auto"` isolates the label for bidi (GH#289): its own direction, and a stored override
 * character cannot reorder the text around the link. A business service (`service`) links to its own page.
 */
const props = withDefaults(defineProps<{ id: string; from?: TrailStep; trail?: TrailStep[]; service?: boolean }>(), {
  from: undefined,
  trail: () => [],
  service: false,
});
const to = computed(() => ({
  path: `${props.service ? "/services" : "/cis"}/${props.id}`,
  state: props.from ? { trail: extendTrail(props.trail, props.from, props.id) } : undefined,
}));
</script>

<template>
  <RouterLink :to="to" dir="auto"><slot /></RouterLink>
</template>
