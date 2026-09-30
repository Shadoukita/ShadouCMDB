<script setup lang="ts">
import type { AttributeReference, EffectiveAttribute } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import LookupValueName from "../../components/LookupValueName.vue";
import { isMultiline } from "../../lib/attributeValues";
import { formatDate, formatDateTime, HIDDEN_CI } from "../../lib/format";
import type { TrailStep } from "../../lib/trail";

/**
 * Read-only rendering of one attribute value by dataType. Reference values are links,
 * except into a class the caller cannot view: that CI would answer 404, so no link.
 */
defineProps<{
  def: EffectiveAttribute;
  value: unknown;
  refInfo?: AttributeReference;
  self: TrailStep;
  trail: TrailStep[];
}>();
const isUrl = (v: unknown) => /^https?:\/\//.test(String(v));
</script>

<!-- User text renders isolated (<bdi>, dir="auto", GH#289): a bidi control allowed in a multiline value
     cannot reorder the unit, label or link next to it. -->
<template>
  <span v-if="value === null || value === undefined || value === ''" class="muted">—</span>
  <template v-else-if="def.dataType === 'boolean'">{{ value ? "Yes" : "No" }}</template>
  <template v-else-if="def.dataType === 'date'">{{ formatDate(String(value)) }}</template>
  <template v-else-if="def.dataType === 'datetime'">{{ formatDateTime(String(value)) }}</template>
  <span v-else-if="def.dataType === 'ip' || def.dataType === 'cidr'" class="mono">{{ String(value) }}</span>
  <span v-else-if="def.dataType === 'reference' && refInfo?.hidden" class="muted" title="You do not have permission to view this configuration item's class">{{ HIDDEN_CI }}</span>
  <CiLink v-else-if="def.dataType === 'reference'" :id="String(value)" :from="self" :trail="trail">
    {{ refInfo?.name ?? String(value) }}{{ refInfo?.deleted ? " (deleted)" : "" }}
  </CiLink>
  <LookupValueName v-else-if="def.dataType === 'lookup'" :list-id="def.lookupListId" :value-id="String(value)" />
  <span v-else-if="isMultiline(def) || (def.dataType === 'text' && String(value).includes('\n'))" class="multiline" dir="auto">{{ String(value) }}</span>
  <a v-else-if="def.dataType === 'text' && isUrl(value)" :href="String(value)" target="_blank" rel="noreferrer noopener" dir="auto">{{ String(value) }}</a>
  <bdi v-else>{{ String(value) }}</bdi>
</template>
