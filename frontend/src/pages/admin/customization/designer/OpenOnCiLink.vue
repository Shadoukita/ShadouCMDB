<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useCiList, type CiClass } from "../../../../api/queries";
import { EDIT_LAYOUT_QUERY, EDIT_LAYOUT_VALUE } from "../../../../lib/layoutEditor";

/**
 * "Open on a CI": the class's layout in edit mode on a real page, its first CI
 * by label, or an empty form of the class when it has none yet.
 */
const props = defineProps<{ cls: CiClass }>();
const first = useCiList(() => ({ classId: props.cls.id, limit: 1, sort: "label" }));
const ci = computed(() => first.data.value?.data[0]);
const to = computed(() =>
  ci.value
    ? { path: `/cis/${ci.value.id}`, query: { [EDIT_LAYOUT_QUERY]: EDIT_LAYOUT_VALUE } }
    : { path: "/cis/new", query: { classId: props.cls.id, [EDIT_LAYOUT_QUERY]: EDIT_LAYOUT_VALUE } },
);
</script>

<template>
  <span class="inline-control">
    <RouterLink v-if="!first.isLoading.value" class="btn" :to="to">Open on a CI</RouterLink>
    <span class="muted">
      <template v-if="first.isLoading.value">Looking for a {{ cls.name }} CI…</template>
      <template v-else-if="ci">Edit the saved layout in place on {{ ci.label }}</template>
      <template v-else>No {{ cls.name }} CI yet: edit the saved layout in place on an empty form</template>
    </span>
  </span>
</template>
