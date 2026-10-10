<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../api/client";
import { useCi } from "../api/queries";
import { t } from "../i18n";
import { hiddenCi } from "../lib/format";

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
    {{ ci.data.value.label }}{{ ci.data.value.deletedAt ? ` ${t("record.deletedSuffix")}` : "" }}
  </RouterLink>
  <span v-else-if="ci.isLoading.value" class="muted">…</span>
  <span v-else-if="hidden" class="muted" :title="t('record.ciName.hidden')">{{ hiddenCi() }}</span>
  <span v-else class="mono muted" :title="t('record.ciName.loadFailed')">{{ id }}</span>
</template>
