<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import type { TrailStep } from "../../lib/trail";
import { attributeKey, fieldLabel, type ResolvedPanel } from "../../lib/uiSettings";
import AttributeValue from "./AttributeValue.vue";
import CoreFieldValue from "./CoreFieldValue.vue";

/**
 * The detail page's fields by panel (lib/uiSettings resolveLayout): the class
 * layout's panels, collapsed ones closed, then General, the attribute groups and
 * the record's class and timestamps. Values no current definition describes
 * (e.g. after a class change) are listed last, so nothing stored is hidden.
 */
const props = defineProps<{ ci: Ci; panels: ResolvedPanel[]; defs: EffectiveAttribute[]; self: TrailStep; trail: TrailStep[] }>();
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const defFor = (field: string) => props.defs.find((d) => d.key === attributeKey(field));
const orphans = computed(() => {
  const known = new Set(props.defs.map((d) => d.key));
  return Object.keys(values.value).filter((k) => !known.has(k) && values.value[k] != null);
});
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
          <template v-if="p.key === '_record'">
            <dt>ID</dt>
            <dd class="mono">{{ ci.id }}</dd>
          </template>
        </dl>
      </div>
    </details>
    <section v-if="orphans.length > 0" class="panel">
      <div class="panel-header"><h2>Not defined by this class</h2></div>
      <div class="panel-body">
        <dl class="props">
          <template v-for="k in orphans" :key="k">
            <dt>{{ k }}</dt>
            <dd><span class="mono">{{ JSON.stringify(values[k]) }}</span></dd>
          </template>
        </dl>
      </div>
    </section>
  </div>
</template>
