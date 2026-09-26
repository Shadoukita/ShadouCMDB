<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import type { TrailStep } from "../../lib/trail";
import { attributeKey, fieldLabel, type ResolvedPanel } from "../../lib/uiSettings";
import AttributeValue from "./AttributeValue.vue";
import CoreFieldValue from "./CoreFieldValue.vue";

/**
 * The detail page's fields arranged by the class's layout (Administration ›
 * Customization › Detail and form layout): the administrator's panels in
 * order, collapsed ones closed, then whatever they do not place.
 */
const props = defineProps<{ ci: Ci; panels: ResolvedPanel[]; defs: EffectiveAttribute[]; self: TrailStep; trail: TrailStep[] }>();
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences as Record<string, { id: string; name: string; deleted?: boolean } | undefined>);
const defFor = (field: string) => props.defs.find((d) => d.key === attributeKey(field));
</script>

<template>
  <div class="layout-panels">
    <details v-for="p in panels" :key="p.key" class="panel layout-panel" :open="!p.collapsed">
      <summary class="panel-header"><h2>{{ p.label }}</h2></summary>
      <div class="panel-body">
        <dl class="props">
          <template v-for="f in p.fields" :key="f">
            <dt>{{ fieldLabel(f, defs) }}</dt>
            <dd>
              <AttributeValue v-if="defFor(f)" :def="defFor(f)!" :value="values[defFor(f)!.key]" :ref-info="refs[defFor(f)!.key]" :self="self" :trail="trail" />
              <CoreFieldValue v-else :ci="ci" :field="f" />
            </dd>
          </template>
        </dl>
      </div>
    </details>
  </div>
</template>
