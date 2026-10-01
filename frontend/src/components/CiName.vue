<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../api/client";
import { useCi } from "../api/queries";
import { HIDDEN_CI } from "../lib/format";

/**
 * A CI named by id alone (e.g. a reference value in the import check): its label as a link. One GET per id, cached,
 * so the same CI shown on many rows is fetched once. A CI the caller may not view answers 403/404: no link.
 */
const props = defineProps<{ id: string }>();
const ci = useCi(() => props.id);
const hidden = computed(() => ci.error.value instanceof ApiError && (ci.error.value.status === 403 || ci.error.value.status === 404));
</script>

<template>
  <RouterLink v-if="ci.data.value" :to="`/cis/${id}`" dir="auto">
    {{ ci.data.value.label }}{{ ci.data.value.deletedAt ? " (deleted)" : "" }}
  </RouterLink>
  <span v-else-if="ci.isLoading.value" class="muted">…</span>
  <span v-else-if="hidden" class="muted" title="The CI does not exist or you do not have permission to view it">{{ HIDDEN_CI }}</span>
  <span v-else class="mono muted" title="Could not load the CI">{{ id }}</span>
</template>
