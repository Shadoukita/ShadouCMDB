<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useCiClasses, useClassAttributes, useCreateCi, type Ci, type CiCreateBody } from "../../api/queries";
import { useCiLayout } from "../../api/uiSettings";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LayoutEditView from "../../components/layoutEdit/LayoutEditView.vue";
import LoadingState from "../../components/LoadingState.vue";
import { useAppSettings } from "../../lib/appSettings";
import { toApiValue } from "../../lib/attributeValues";
import { coreToApi } from "../../lib/ciEdits";
import type { LayoutEditor } from "../../lib/layoutEditor";
import { asClassLayout } from "../../lib/layoutTemplates";
import {
  builtInLayout,
  CORE_FIELDS,
  freeAreaStyle,
  gridClass,
  layoutFor,
  normalizeLayout,
  PANELS,
  resolveLayout,
  sectionClass,
  sectionStyle,
  windowClass,
  windowStyle,
  withoutKinds,
  type ResolvedSection,
} from "../../lib/uiSettings";
import { useFlashStore } from "../../stores/flash";
import NoteText from "../../components/NoteText.vue";
import CiFieldInput from "./CiFieldInput.vue";
import { fieldIdFor, useCiDraft } from "./ciDraft";
import FormErrorBanner from "./FormErrorBanner.vue";

/**
 * The CI form. Core fields (ident, validity period) are the same for every CI; everything
 * else, name and status included, is a class attribute rendered from
 * GET /ci-classes/{id}/attributes, so a new class needs no frontend change.
 * The parent keys this component by class (create) or id+version (edit). The values
 * being edited are a CI draft (ciDraft.ts), shared with the detail page.
 *
 * Every class starts with a General section: ident, valid from, valid until and
 * the attributes without a group; the other attribute groups follow as sections.
 * A class layout (Administration › Customization › Detail and form layout) arranges
 * the fields in tabs and sections (windows), hides fields and makes fields
 * read-only. Required fields stay editable on a new CI whatever the layout says,
 * or it could never be saved. Every tab stays in the page (only one is shown),
 * so the whole form is submitted and a tab holding an error says so.
 *
 * In layout edit mode (`editor` active, see lib/layoutEditor) the form's fields
 * are shown on the layout canvas instead, inert, with what has been typed so far.
 */
const props = defineProps<{ mode: "create" | "edit"; classId: string; className: string; ci?: Ci; editor?: LayoutEditor }>();

const router = useRouter();
const route = useRoute();
/** Created from the business service list: back there on Cancel, on to the service (Owners open) on save. */
const fromServices = computed(() => props.mode === "create" && route.query.return === "services");
const flash = useFlashStore();
const attrs = useClassAttributes(() => props.classId);
const create = useCreateCi();

const settings = useAppSettings();
const classes = useCiClasses();
const classKey = computed(() => classes.data.value?.find((c) => c.id === props.classId)?.key);
/** An existing CI's layout: its own, a template chosen for it, or its class's default (a new CI gets the class's). */
const ciLayout = useCiLayout(() => (props.mode === "edit" ? props.ci?.id : undefined));
/** The layout; in layout edit mode the draft, so the fields show what is being set (read-only). */
const layout = computed(() => {
  if (props.editor?.active && props.editor.layout) return props.editor.layout;
  const own = ciLayout.data.value;
  if (own && own.ciId === props.ci?.id) return normalizeLayout(asClassLayout(own.classKey, own.layout));
  return layoutFor(settings.doc.value, classKey.value);
});
const draft = useCiDraft({
  mode: props.mode,
  classId: () => props.classId,
  ci: () => props.ci,
  attrs: () => attrs.data.value,
  readOnlyFields: () => layout.value?.readOnlyFields,
});
const pending = computed(() => (props.mode === "create" ? create.isPending.value : draft.pending));
const activeAttrs = computed(() => attrs.data.value?.filter((d) => d.isActive));
const requiredField = (f: string) => !!draft.defs.find((d) => `attributes.${d.key}` === f && d.isRequired && d.isActive);
const keepEditable = (f: string) => props.mode === "create" && requiredField(f);

/**
 * The class's layout, or the built-in one: General (core fields and ungrouped attributes), then the attribute groups.
 * Notes show on the form too; the built-in panels are the detail page's.
 */
const tabs = computed(() => {
  const l = withoutKinds(layout.value ?? builtInLayout(""), PANELS.map((p) => p.kind));
  return resolveLayout({ ...l, hiddenFields: (l.hiddenFields ?? []).filter((f) => !keepEditable(f)) }, draft.defs, CORE_FIELDS);
});
/** A tab's sections: the windows (lib/freeLayout), then everything the layout does not place. */
function sectionGroups(sections: readonly ResolvedSection[]) {
  const windows = sections.filter((sec) => sec.frame);
  const flow = sections.filter((sec) => !sec.frame);
  return windows.length > 0 ? [{ free: true, items: windows }, { free: false, items: flow }] : [{ free: false, items: flow }];
}
/** The first section of fields, which also shows whether the attributes loaded. */
const firstGrid = computed(() => tabs.value[0]?.sections.find((sec) => sec.kind === "fields")?.key);
const activeTab = ref(0);
const tabIndex = computed(() => Math.min(activeTab.value, tabs.value.length - 1));
const tabFields = (i: number) => tabs.value[i]?.sections.flatMap((sec) => sec.fields.map((c) => c.field)) ?? [];
const tabErrorCount = (i: number) => tabFields(i).filter((f) => draft.fieldErrors[f]).length;
/** Arrow keys, Home and End move between the tabs (the tab list is one stop in the tab order). */
function onTabKey(e: KeyboardEvent) {
  const n = tabs.value.length;
  const to = { ArrowRight: tabIndex.value + 1, ArrowLeft: tabIndex.value - 1 + n, Home: 0, End: n - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  activeTab.value = to % n;
  void nextTick(() => document.getElementById(`form-tab-${tabs.value[activeTab.value].key}`)?.focus());
}
/** Shows the tab holding `field` and puts the cursor in it. */
async function focusField(field: string) {
  const i = tabs.value.findIndex((_, j) => tabFields(j).includes(field));
  if (i >= 0) activeTab.value = i;
  await nextTick();
  document.getElementById(fieldIdFor(field))?.focus();
}

async function onSubmit() {
  draft.error = null;
  // Catch empty required fields before the round trip; everything else is validated by the API.
  const missing = draft.checkRequired(new Set(["validFrom", ...tabs.value.flatMap((_, i) => tabFields(i))]));
  if (missing.length > 0) {
    await focusField(missing[0]);
    return;
  }
  try {
    if (props.mode === "create") {
      const attributes: Record<string, unknown> = {};
      for (const d of draft.defs) {
        const cur = draft.values[d.key] ?? "";
        if (cur !== "") attributes[d.key] = toApiValue(d, cur);
      }
      // Empty core fields are left out: the API generates the ident and starts the validity period now.
      const given = Object.fromEntries(Object.entries(coreToApi(draft.core)).filter(([k, v]) => v !== null && (k !== "ident" || draft.isAdmin)));
      const body = { classId: props.classId, ...given, ...(draft.criticalityId ? { criticalityValueId: draft.criticalityId } : {}), attributes } as CiCreateBody;
      const created = await create.mutateAsync(body);
      flash.show(created.id, `Created ${created.label}.`);
      await router.push(fromServices.value ? { path: `/services/${created.id}`, query: { edit: "owners" } } : `/cis/${created.id}`);
    } else if (props.ci) {
      const saved = await draft.save();
      if (saved) flash.show(saved.id, `Saved ${saved.label}.`);
      await router.push(`/cis/${props.ci.id}`);
    }
  } catch (err) {
    draft.error = err;
    // Show the first tab with a rejected field, so the message next to it is in view.
    const withError = tabs.value.findIndex((_, i) => tabErrorCount(i) > 0);
    if (withError >= 0) activeTab.value = withError;
    window.scrollTo({ top: 0 });
  }
}
</script>

<template>
  <LayoutEditView v-if="editor?.active && classKey" :editor="editor" :class-name="className" :attrs="activeAttrs" :attrs-error="attrs.error.value" form>
    <template #field="{ field }"><CiFieldInput :draft="draft" :f="field" /></template>
  </LayoutEditView>
  <form v-else novalidate :aria-label="mode === 'create' ? `New ${className}` : `Edit ${ci?.label}`" @submit.prevent="onSubmit">
    <FormErrorBanner v-if="draft.error != null" :error="draft.error" :unplaced="draft.unplaced" :version-conflict-href="ci ? `/cis/${ci.id}` : undefined" />
    <div class="layout-container">
      <div v-if="tabs.length > 1" class="tabs" role="tablist" aria-label="Form tabs">
        <button
          v-for="(t, i) in tabs"
          :id="`form-tab-${t.key}`"
          :key="t.key"
          type="button"
          role="tab"
          :aria-selected="i === tabIndex"
          :aria-controls="`form-tabpanel-${t.key}`"
          :tabindex="i === tabIndex ? 0 : -1"
          @click="activeTab = i"
          @keydown="onTabKey"
        >
          {{ t.label }}<span v-if="tabErrorCount(i) > 0" class="badge danger tab-errors">{{ tabErrorCount(i) }} error{{ tabErrorCount(i) === 1 ? "" : "s" }}</span>
        </button>
      </div>
      <div
        v-for="(t, i) in tabs"
        v-show="i === tabIndex"
        :id="`form-tabpanel-${t.key}`"
        :key="t.key"
        :role="tabs.length > 1 ? 'tabpanel' : undefined"
        :aria-labelledby="tabs.length > 1 ? `form-tab-${t.key}` : undefined"
      >
        <div v-for="g in sectionGroups(t.sections)" :key="String(g.free)" :class="g.free ? 'lg-free' : 'layout-panels'" :style="g.free ? freeAreaStyle(g.items) : undefined">
          <details
            v-for="sec in g.items"
            :key="sec.key"
            :class="['panel', 'layout-panel', ...(sec.frame ? [windowClass] : sectionClass(sec))]"
            :style="sec.frame ? windowStyle(sec.frame) : sectionStyle(sec)"
            :data-section="sec.key"
            :open="!sec.collapsed"
          >
            <summary class="panel-header">
              <h2>{{ sec.label }}</h2>
            </summary>
            <div v-if="sec.kind === 'note'" class="panel-body"><NoteText :text="sec.text ?? ''" /></div>
            <div v-else class="panel-body">
              <template v-if="i === 0 && sec.key === firstGrid">
                <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
                <ErrorAlert
                  v-if="attrs.isError.value"
                  :error="attrs.error.value"
                  title="Could not load this class's attributes"
                  :on-retry="() => attrs.refetch()"
                />
              </template>
              <div :class="gridClass(sec.columns)">
                <CiFieldInput v-for="{ field: f, width } in sec.fields" :key="f" :draft="draft" :f="f" :width="width" :columns="sec.columns" />
              </div>
            </div>
          </details>
        </div>
      </div>
    </div>
    <div class="panel form-footer">
      <button type="submit" class="btn btn-primary" :disabled="pending || attrs.isLoading.value || attrs.isError.value">
        {{ pending ? "Saving…" : mode === "create" ? `Create ${className}` : "Save changes" }}
      </button>
      <RouterLink class="btn" :to="ci ? `/cis/${ci.id}` : fromServices ? '/services' : '/cis'">Cancel</RouterLink>
    </div>
  </form>
</template>
