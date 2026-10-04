<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import type { Ci, CiClass, EffectiveAttribute } from "../api/queries";
import { formatDate, formatDateTime, formatRelative } from "../lib/format";
import { attributeKey } from "../lib/uiSettings";
import AttributeValue from "../pages/detail/AttributeValue.vue";
import CiStateBadge from "./CiStateBadge.vue";
import ClassBadge from "./ClassBadge.vue";
import CriticalityBadge from "./CriticalityBadge.vue";

/**
 * One inventory cell: a built-in field or `attributes.<key>`, as chosen by the
 * class's list view (Administration › Customization › List views).
 */
const props = defineProps<{
  ci: Ci;
  field: string;
  defs: readonly EffectiveAttribute[];
  /** The class catalogue, for the class column's icon (audit I8). Without it the column shows the name only. */
  classOf?: (id: string) => CiClass | undefined;
}>();
const cls = computed(() => (props.field === "class" ? props.classOf?.(props.ci.classId) : undefined));
const attr = computed(() => attributeKey(props.field));
const def = computed(() => (attr.value ? props.defs.find((d) => d.key === attr.value) : undefined));
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const self = computed(() => ({ id: props.ci.id, name: props.ci.label }));
</script>

<template>
  <template v-if="attr !== null">
    <AttributeValue v-if="def" :def="def" :value="values?.[attr]" :ref-info="refs?.[attr]" :self="self" :trail="[]" />
    <span v-else class="muted">—</span>
  </template>
  <RouterLink v-else-if="field === 'label'" :to="`/cis/${ci.id}`" dir="auto">{{ ci.label }}</RouterLink>
  <span v-else-if="field === 'ident'" class="mono">{{ ci.ident }}</span>
  <ClassBadge v-else-if="field === 'class' && cls" :icon="cls.icon" :color="cls.color" :name="ci.class.name" />
  <bdi v-else-if="field === 'class'">{{ ci.class.name }}</bdi>
  <template v-else-if="field === 'criticality'">
    <CriticalityBadge v-if="ci.criticality" :value="ci.criticality" /><span v-else class="muted">—</span>
  </template>
  <CiStateBadge v-else-if="field === 'active'" :ci="ci" show-active />
  <span v-else-if="field === 'validFrom'" :title="formatDateTime(ci.validFrom)">{{ formatDate(ci.validFrom) }}</span>
  <template v-else-if="field === 'validUntil'">
    <span v-if="ci.validUntil" :title="formatDateTime(ci.validUntil)">{{ formatDate(ci.validUntil) }}</span><span v-else class="muted">—</span>
  </template>
  <span v-else-if="field === 'updatedAt'" :title="formatDateTime(ci.updatedAt)">{{ formatRelative(ci.updatedAt) }}</span>
  <span v-else-if="field === 'createdAt'" :title="formatDateTime(ci.createdAt)">{{ formatRelative(ci.createdAt) }}</span>
</template>
