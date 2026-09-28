<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import type { TrailStep } from "../../lib/trail";
import { attributeKey, cellClass, fieldLabel, gridClass, sectionClass, sectionStyle, type ResolvedSection } from "../../lib/uiSettings";
import AttributeValue from "./AttributeValue.vue";
import BlockContent from "./BlockContent.vue";
import CoreFieldValue from "./CoreFieldValue.vue";

/**
 * One tab of the detail page's fields (lib/uiSettings resolveLayout): its
 * sections on the tab's 12-column grid (side by side where their widths allow
 * it, stacked on small screens), collapsed ones closed, each a grid of label/value cells as wide as
 * the class layout says. On the first tab the built-in sections follow: General,
 * the attribute groups and the record's class and timestamps. Notes and the
 * built-in panels a layout places are sections too (BlockContent). Values no current
 * definition describes (e.g. after a class change) are listed last on the first
 * tab (`orphans`), so nothing stored is hidden.
 */
const props = defineProps<{
  ci: Ci;
  sections: ResolvedSection[];
  defs: EffectiveAttribute[];
  self: TrailStep;
  trail: TrailStep[];
  orphans?: boolean;
}>();
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const defFor = (field: string) => props.defs.find((d) => d.key === attributeKey(field));
const orphanKeys = computed(() => {
  if (!props.orphans) return [];
  const known = new Set(props.defs.map((d) => d.key));
  return Object.keys(values.value).filter((k) => !known.has(k) && values.value[k] != null);
});
</script>

<template>
  <div class="layout-container">
    <div class="layout-panels">
      <details v-for="p in sections" :key="p.key" :class="['panel', 'layout-panel', ...sectionClass(p)]" :style="sectionStyle(p)" :data-section="p.key" :open="!p.collapsed">
        <summary class="panel-header"><h2>{{ p.label }}</h2></summary>
        <BlockContent v-if="p.kind !== 'fields'" :kind="p.kind" :text="p.text" :ci="ci" :self="self" :trail="trail" />
        <div v-else class="panel-body">
          <dl :class="gridClass(p.columns)">
            <div v-for="{ field: f, width } in p.fields" :key="f" :class="['prop', cellClass(width, p.columns)]">
              <dt>{{ fieldLabel(f, defs) }}</dt>
              <dd>
                <AttributeValue v-if="defFor(f)" :def="defFor(f)!" :value="values[defFor(f)!.key]" :ref-info="refs[defFor(f)!.key]" :self="self" :trail="trail" />
                <CoreFieldValue v-else :ci="ci" :field="f" />
              </dd>
            </div>
            <div v-if="p.key === '_record'" :class="['prop', cellClass(1, p.columns)]">
              <dt>ID</dt>
              <dd class="mono">{{ ci.id }}</dd>
            </div>
          </dl>
        </div>
      </details>
      <section v-if="orphanKeys.length > 0" class="panel">
        <div class="panel-header"><h2>Not defined by this class</h2></div>
        <div class="panel-body">
          <dl class="props">
            <template v-for="k in orphanKeys" :key="k">
              <dt>{{ k }}</dt>
              <dd><span class="mono">{{ JSON.stringify(values[k]) }}</span></dd>
            </template>
          </dl>
        </div>
      </section>
    </div>
  </div>
</template>
