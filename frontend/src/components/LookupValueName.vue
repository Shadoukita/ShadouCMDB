<script setup lang="ts">
import { computed } from "vue";
import { useLookupListValues } from "../api/datamodel";
import { t } from "../i18n";

/** Read-only name of a lookup list value (CI detail), with its colour. The list is cached per list id. */
const props = defineProps<{ listId: string | null; valueId: string }>();
const values = useLookupListValues(() => props.listId);
const value = computed(() => values.data.value?.find((v) => v.id === props.valueId));
</script>

<template>
  <span v-if="value" class="class-badge lookup-value" :class="{ coloured: !!value.color }" :style="value.color ? { '--swatch': value.color } : undefined">
    <span v-if="value.color" class="class-swatch" aria-hidden="true" :style="{ background: value.color }" />
    <bdi>{{ value.name }}</bdi><span v-if="!value.isActive" class="muted"> {{ t("attr.value.retired") }}</span>
  </span>
  <span v-else-if="values.isLoading.value" class="muted">…</span>
  <span v-else class="mono muted" :title="values.isError.value ? t('attr.value.listLoadFailed') : t('attr.value.unknown')">{{ valueId }}</span>
</template>
