<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useLookupListValues } from "../../api/datamodel";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import { t } from "../../i18n";

/** One fact chip: the attribute's label and the lookup value's name, linking to the inventory filtered by that value. */
const props = defineProps<{ ci: Ci; def: EffectiveAttribute; valueId: string }>();
const values = useLookupListValues(() => props.def.lookupListId);
const name = computed(() => values.data.value?.find((v) => v.id === props.valueId)?.name);
const to = computed(() => ({ path: "/cis", query: { classId: props.ci.classId, lookupValueId: props.valueId } }));
</script>

<template>
  <RouterLink
    v-if="name"
    class="chip fact-chip"
    :to="to"
    :title="t('record.facts.filter', { class: ci.class.name, key: def.label, value: name })"
  >
    <span class="key" dir="auto">{{ def.label }}:</span><bdi class="value">{{ name }}</bdi>
  </RouterLink>
</template>
