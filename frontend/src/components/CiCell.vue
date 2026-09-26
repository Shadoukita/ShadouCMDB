<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import type { Ci, EffectiveAttribute } from "../api/queries";
import { formatDateTime, formatRelative } from "../lib/format";
import { attributeKey } from "../lib/uiSettings";
import AttributeValue from "../pages/detail/AttributeValue.vue";
import StatusBadge from "./StatusBadge.vue";

/**
 * One inventory cell: a built-in field or `attributes.<key>`, as chosen by the
 * class's list view (Administration › Customization › List views).
 */
const props = defineProps<{ ci: Ci; field: string; defs: readonly EffectiveAttribute[] }>();
const attr = computed(() => attributeKey(props.field));
const def = computed(() => (attr.value ? props.defs.find((d) => d.key === attr.value) : undefined));
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences as Record<string, { id: string; name: string; deleted?: boolean } | undefined>);
const self = computed(() => ({ id: props.ci.id, name: props.ci.name }));
</script>

<template>
  <template v-if="attr !== null">
    <AttributeValue v-if="def" :def="def" :value="values?.[attr]" :ref-info="refs?.[attr]" :self="self" :trail="[]" />
    <span v-else class="muted">—</span>
  </template>
  <RouterLink v-else-if="field === 'name'" :to="`/cis/${ci.id}`">{{ ci.name }}</RouterLink>
  <template v-else-if="field === 'class'">{{ ci.class.name }}</template>
  <template v-else-if="field === 'status'">
    <span v-if="ci.deletedAt" class="badge danger">Deleted</span>
    <StatusBadge v-else :status="ci.status" />
  </template>
  <template v-else-if="field === 'environment' || field === 'owner' || field === 'location'">
    <template v-if="ci[field]">{{ ci[field]!.name }}</template><span v-else class="muted">—</span>
  </template>
  <span v-else-if="field === 'hostname' || field === 'ipAddress' || field === 'serialNumber'" class="mono">{{ ci[field] ?? "" }}</span>
  <span v-else-if="field === 'notes'" class="cell-clip" :title="ci.notes ?? undefined">{{ ci.notes ?? "" }}</span>
  <span v-else-if="field === 'updatedAt'" :title="formatDateTime(ci.updatedAt)">{{ formatRelative(ci.updatedAt) }}</span>
  <span v-else-if="field === 'createdAt'" :title="formatDateTime(ci.createdAt)">{{ formatRelative(ci.createdAt) }}</span>
</template>
