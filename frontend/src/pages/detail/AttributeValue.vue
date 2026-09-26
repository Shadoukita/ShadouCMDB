<script setup lang="ts">
import type { EffectiveAttribute } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import LookupValueName from "../../components/LookupValueName.vue";
import { formatDate, formatDateTime } from "../../lib/format";
import type { TrailStep } from "../../lib/trail";

/** Read-only rendering of one attribute value by dataType. Reference values are links. */
defineProps<{
  def: EffectiveAttribute;
  value: unknown;
  refInfo?: { id: string; name: string; deleted?: boolean };
  self: TrailStep;
  trail: TrailStep[];
}>();
const isUrl = (v: unknown) => /^https?:\/\//.test(String(v));
</script>

<template>
  <span v-if="value === null || value === undefined || value === ''" class="muted">—</span>
  <template v-else-if="def.dataType === 'boolean'">{{ value ? "Yes" : "No" }}</template>
  <template v-else-if="def.dataType === 'date'">{{ formatDate(String(value)) }}</template>
  <template v-else-if="def.dataType === 'datetime'">{{ formatDateTime(String(value)) }}</template>
  <span v-else-if="def.dataType === 'ip' || def.dataType === 'cidr'" class="mono">{{ String(value) }}</span>
  <CiLink v-else-if="def.dataType === 'reference'" :id="String(value)" :from="self" :trail="trail">
    {{ refInfo?.name ?? String(value) }}{{ refInfo?.deleted ? " (deleted)" : "" }}
  </CiLink>
  <LookupValueName v-else-if="def.dataType === 'lookup'" :list-id="def.lookupListId" :value-id="String(value)" />
  <a v-else-if="def.dataType === 'text' && isUrl(value)" :href="String(value)" target="_blank" rel="noreferrer noopener">{{ String(value) }}</a>
  <template v-else>{{ String(value) }}</template>
</template>
