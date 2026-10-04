<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import { t } from "../../i18n";
import FactChip from "./FactChip.vue";

/**
 * The fact chips under a CI's title (design §2.7, record page): one `key:value` chip per lookup attribute that
 * has a value, in the class's attribute order. They come from the class's attribute definitions, never from a
 * list per class, and each links to the inventory filtered by that value (`lookupValueId`), which is the
 * filter the explorer's facets and query bar use.
 */
const props = defineProps<{ ci: Ci; defs: EffectiveAttribute[] }>();
const facts = computed(() =>
  props.defs
    .filter((d) => d.dataType === "lookup" && d.lookupListId && props.ci.attributes[d.key] != null && props.ci.attributes[d.key] !== "")
    .map((d) => ({ def: d, valueId: String(props.ci.attributes[d.key]) })),
);
</script>

<template>
  <ul v-if="facts.length > 0" class="fact-chips" :aria-label="t('record.facts')" data-testid="fact-chips">
    <li v-for="f in facts" :key="f.def.key">
      <FactChip :ci="ci" :def="f.def" :value-id="f.valueId" />
    </li>
  </ul>
</template>
