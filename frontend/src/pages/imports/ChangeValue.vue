<script setup lang="ts">
import type { EffectiveAttribute } from "../../api/queries";
import CiName from "../../components/CiName.vue";
import LookupValueName from "../../components/LookupValueName.vue";
import { changeText } from "../../lib/importMapping";

/**
 * One side of a planned change. Lookup and reference attributes are stored as ids (GH#360): show the value's name
 * and the referenced CI's label instead.
 */
defineProps<{ def?: EffectiveAttribute; value: unknown }>();
const isId = (v: unknown): v is string => typeof v === "string" && v !== "";
</script>

<template>
  <LookupValueName v-if="def?.dataType === 'lookup' && isId(value)" :list-id="def.lookupListId" :value-id="value" />
  <CiName v-else-if="def?.dataType === 'reference' && isId(value)" :id="value" />
  <template v-else>{{ changeText(value) }}</template>
</template>
