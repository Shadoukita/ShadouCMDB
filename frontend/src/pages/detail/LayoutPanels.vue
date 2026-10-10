<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import type { TrailStep } from "../../lib/trail";
import { attributeKey, cellClass, fieldLabel, freeAreaStyle, gridClass, isSeparator, sectionClass, sectionStyle, windowClass, windowStyle, type ResolvedSection } from "../../lib/uiSettings";
import type { CiDraft } from "../form/ciDraft";
import CiFieldInput from "../form/CiFieldInput.vue";
import AttributeValue from "./AttributeValue.vue";
import RecordSection from "../../components/RecordSection.vue";
import BlockContent from "./BlockContent.vue";
import CoreFieldValue from "./CoreFieldValue.vue";
import { t } from "../../i18n";

/**
 * One tab of the detail page's fields (lib/uiSettings resolveLayout): its
 * sections as windows where the layout puts them (lib/freeLayout; stacked on
 * small screens), collapsed ones closed (RecordSection), each a grid of label/value cells as wide as
 * the class layout says, with the layout's separators as lines across it. On the
 * first tab the built-in sections follow: General and the attribute groups (and,
 * without a layout, the record details). The record details (the CI's ID, class
 * and timestamps) are a grid like the fields wherever the layout puts them; notes
 * and the other built-in panels are sections too (BlockContent). Values no current
 * definition describes (e.g. after a class change) are listed last on the first
 * tab (`orphans`), so nothing stored is hidden; values of archived fields
 * (`archived`) are listed there too, apart and labelled. The sections the layout
 * does not place follow below the windows at the full width.
 *
 * `stacked` (the built-in arrangement, design §0 step 12d): the sections share one card, each under an
 * overline heading and a rule, as the default layout's field groups.
 *
 * The fields are the CI's form (SHAA-1644, ciDraft.ts): each field the draft may change is an
 * input bound to it; the others (the class, the timestamps, a read-only or managed field, or every
 * field of a CI the user may not edit) show their value in the same place, read-only.
 */
const props = defineProps<{
  ci: Ci;
  sections: ResolvedSection[];
  defs: EffectiveAttribute[];
  self: TrailStep;
  trail: TrailStep[];
  orphans?: boolean;
  /** Archived definitions this CI still holds a value for. */
  archived?: EffectiveAttribute[];
  /** The CI's values being edited. */
  draft: CiDraft;
  /** The sections in one card (the built-in arrangement). */
  stacked?: boolean;
}>();
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const defFor = (field: string) => props.defs.find((d) => d.key === attributeKey(field));
const roId = (field: string) => `ro-${field.replace(/[^\w-]/g, "-")}`;
const roHint = (field: string) => props.draft.readOnlyHint(field);
/** A section holding a field with an error stays open, so the message next to the field is in view. */
const hasError = (p: ResolvedSection) => p.fields.some((f) => !!props.draft.fieldErrors[f.field]);
/** The windows, then everything the layout does not place. */
const groups = computed(() => {
  const windows = props.sections.filter((p) => p.frame);
  const flow = props.sections.filter((p) => !p.frame);
  return windows.length > 0 ? [{ free: true, items: windows }, { free: false, items: flow }] : [{ free: false, items: flow }];
});
const archivedDefs = computed(() => (props.orphans ? (props.archived ?? []) : []));
const orphanKeys = computed(() => {
  if (!props.orphans) return [];
  const known = new Set([...props.defs, ...(props.archived ?? [])].map((d) => d.key));
  return Object.keys(values.value).filter((k) => !known.has(k) && values.value[k] != null);
});
</script>

<template>
  <div :class="['layout-container', { 'lp-stacked': stacked }]">
    <div v-for="g in groups" :key="String(g.free)" :class="g.free ? 'lg-free' : 'layout-panels'" :style="g.free ? freeAreaStyle(g.items) : undefined">
      <RecordSection
        v-for="p in g.items"
        :key="p.key"
        :label="p.label"
        :collapsed="p.collapsed"
        :force-open="hasError(p)"
        :class="p.frame ? [windowClass] : sectionClass(p)"
        :style="p.frame ? windowStyle(p.frame) : sectionStyle(p)"
        :data-section="p.key"
      >
        <BlockContent v-if="p.kind !== 'fields' && p.kind !== 'record'" :kind="p.kind" :text="p.text" :ci="ci" :self="self" :trail="trail" />
        <div v-else class="panel-body">
          <div :class="gridClass(p.columns)">
            <template v-for="(item, i) in p.items" :key="isSeparator(item) ? `sep-${i}` : item.field">
              <div v-if="isSeparator(item)" class="lg-sep" role="separator" :aria-label="item.label" data-separator>
                <span v-if="item.label" class="lg-sep-label" dir="auto">{{ item.label }}</span>
              </div>
              <CiFieldInput v-else-if="draft.editable(item.field)" :draft="draft" :f="item.field" :width="item.width" :columns="p.columns" :ci="ci" :self="self" :trail="trail" />
              <div v-else :class="['field', 'field-ro', cellClass(item.width, p.columns)]" :data-field="item.field">
                <span :id="roId(item.field)" class="label">{{ fieldLabel(item.field, defs) }}</span>
                <div class="ro-value" role="group" :aria-labelledby="roId(item.field)">
                  <AttributeValue v-if="defFor(item.field)" :def="defFor(item.field)!" :value="values[defFor(item.field)!.key]" :ref-info="refs[defFor(item.field)!.key]" :self="self" :trail="trail" />
                  <CoreFieldValue v-else :ci="ci" :field="item.field" />
                </div>
                <span v-if="roHint(item.field)" class="hint" dir="auto">{{ roHint(item.field) }}</span>
              </div>
            </template>
            <div v-if="p.kind === 'record'" :class="['field', 'field-ro', cellClass(1, p.columns)]" data-field="id">
              <span id="ro-id" class="label">ID</span>
              <div class="ro-value mono" role="group" aria-labelledby="ro-id">{{ ci.id }}</div>
            </div>
          </div>
        </div>
      </RecordSection>
      <section v-if="!g.free && archivedDefs.length > 0" class="panel" data-section="_archived">
        <div class="panel-header"><h2>{{ t("record.archived.title") }}</h2></div>
        <div class="panel-body">
          <p class="hint no-margin-top">{{ t("record.archived.hint") }}</p>
          <div :class="gridClass(3)">
            <div v-for="d in archivedDefs" :key="d.key" :class="['field', 'field-ro', cellClass(1, 3)]" :data-field="`archived.${d.key}`">
              <span :id="roId(`archived.${d.key}`)" class="label" dir="auto">{{ d.label }} <span class="muted">{{ t("record.archived.suffix") }}</span></span>
              <div class="ro-value" role="group" :aria-labelledby="roId(`archived.${d.key}`)">
                <AttributeValue :def="d" :value="values[d.key]" :ref-info="refs[d.key]" :self="self" :trail="trail" />
              </div>
            </div>
          </div>
        </div>
      </section>
      <section v-if="!g.free && orphanKeys.length > 0" class="panel">
        <div class="panel-header"><h2>{{ t("record.orphans.title") }}</h2></div>
        <div class="panel-body">
          <div :class="gridClass(3)">
            <div v-for="k in orphanKeys" :key="k" :class="['field', 'field-ro', cellClass(1, 3)]">
              <span :id="roId(`orphan.${k}`)" class="label mono">{{ k }}</span>
              <div class="ro-value mono" role="group" :aria-labelledby="roId(`orphan.${k}`)">{{ JSON.stringify(values[k]) }}</div>
            </div>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>
