<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useAreas } from "../api/datamodel";
import { useCi, useCiClasses, useClassAttributes } from "../api/queries";
import { useCiWorkflows } from "../api/workflowRuntime";
import { t } from "../i18n";
import { cloneClearedKeys, type CloneSource } from "../lib/ciClone";
import { dataModelEmpty } from "../lib/dataModel";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import ClassBadge from "../components/ClassBadge.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import EditLayoutButton from "../components/layoutEdit/EditLayoutButton.vue";
import { useDocumentTitle } from "../lib/composables";
import { groupByArea } from "../lib/areas";
import { useLayoutEditor } from "../lib/layoutEditor";
import { vAutofocus } from "../lib/directives";
import { useSessionStore } from "../stores/session";
import CiForm from "./form/CiForm.vue";

const route = useRoute();
const router = useRouter();
const classId = computed(() => (typeof route.query.classId === "string" ? route.query.classId : ""));
const classes = useCiClasses();
const cls = computed(() => classes.data.value?.find((c) => c.id === classId.value));

const session = useSessionStore();
const concrete = computed(() =>
  (classes.data.value ?? []).filter((c) => c.isActive && !c.isAbstract && session.canOnClass(c.id, "create")),
);
const areas = useAreas();
const concreteGroups = computed(() => groupByArea(concrete.value, (c) => c.areaId, areas.data.value ?? []));
const area = computed(() => areas.data.value?.find((a) => a.id === cls.value?.areaId));
const denied = computed(() => !!cls.value && !session.canOnClass(cls.value.id, "create"));
/** Archived classes accept no new CIs; abstract ones hold none. */
const closed = computed(() => !!cls.value && (!cls.value.isActive || cls.value.isAbstract));
const fromServices = computed(() => route.query.return === "services");
const crumbs = computed<Crumb[]>(() =>
  fromServices.value
    ? [{ label: t("services.title"), to: "/services" }, { label: t("services.new") }]
    : [
        { label: "Inventory", to: "/cis" },
        ...(area.value ? [{ label: area.value.name }] : []),
        ...(cls.value ? [{ label: cls.value.name, to: `/cis?classId=${cls.value.id}` }] : []),
        { label: "New" },
      ],
);

// Edit layout (the layout-editor route, in its own window): the form's layout edited on an empty form of the class,
// also where the designer's "Open on a CI" leads for a class without CIs.
const attrs = useClassAttributes(() => cls.value?.id);
const editor = useLayoutEditor({ classKey: () => cls.value?.key, attrs: () => attrs.data.value?.filter((d) => d.isActive) });

const unknownClass = computed(() => new Error(`Class ${classId.value} does not exist.`));

// Clone (?cloneFrom=<id>, gap G11): the form starts from that CI's values (lib/ciClone). The source is read through
// the API like any CI the user may view; the new CI is created through the normal create path.
const cloneFrom = computed(() => (typeof route.query.cloneFrom === "string" ? route.query.cloneFrom : ""));
const source = useCi(cloneFrom);
const sourceWorkflows = useCiWorkflows(() => (source.data.value ? cloneFrom.value : undefined));
const sourceOtherClass = computed(() => !!source.data.value && source.data.value.classId !== classId.value);
const cleared = computed(() => cloneClearedKeys(attrs.data.value?.filter((d) => d.isActive) ?? [], cls.value?.titleAttributeId, source.data.value?.attributeReferences));
const clearedLabels = computed(() => cleared.value.map((k) => attrs.data.value?.find((d) => d.key === k)?.label ?? k));
/** The clone's source once everything it needs has loaded (a failed workflow lookup resets nothing; the API reports a refused value). */
const clone = computed<(CloneSource & { ci: NonNullable<typeof source.data.value> }) | undefined>(() => {
  const ci = source.data.value;
  if (!ci || sourceOtherClass.value || !attrs.data.value || sourceWorkflows.isLoading.value) return undefined;
  return { ci, attributes: ci.attributes, cleared: new Set(cleared.value), reset: new Set(sourceWorkflows.data.value?.controlledFields ?? []) };
});
const cloneLoading = computed(() => !!cloneFrom.value && !sourceOtherClass.value && !source.isError.value && !clone.value);
useDocumentTitle(() => (clone.value ? t("clone.title", { name: clone.value.ci.label }) : cls.value ? `New ${cls.value.name}` : "New CI"));

function pickClass(e: Event) {
  const value = (e.target as HTMLSelectElement).value;
  router.replace({ path: "/cis/new", query: value ? { classId: value } : {} });
}
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <div class="page-header record-header">
    <div class="record-heading">
      <div class="title">
        <ClassBadge v-if="cls" :icon="cls.icon" :color="cls.color" />
        <h1 dir="auto">{{ cls && !closed && !denied ? `New ${cls.name}` : "New configuration item" }}</h1>
      </div>
      <p v-if="area || cls" class="record-meta">
        <span v-if="area" dir="auto">{{ area.name }}</span>
        <span v-if="area && cls" class="sep" aria-hidden="true">·</span>
        <RouterLink v-if="cls" :to="`/cis?classId=${cls.id}`" dir="auto">{{ cls.name }}</RouterLink>
      </p>
    </div>
    <div v-if="editor.allowed && !editor.active && cls" class="actions">
      <EditLayoutButton :editor="editor" />
    </div>
  </div>
  <!-- Without a class picked: a link straight to a built-in class (Business service) still opens its form. -->
  <section v-if="!classId && classes.data.value && dataModelEmpty(classes.data.value)" class="panel callout">
    <DataModelEmpty />
  </section>
  <section v-else class="panel record-class-picker">
    <div class="panel-body">
      <div class="field">
        <label for="ci-class">Class<span class="req" aria-hidden="true">*</span></label>
        <ErrorAlert v-if="classes.isError.value" :error="classes.error.value" :on-retry="() => classes.refetch()" />
        <select v-else id="ci-class" v-autofocus="!classId" :value="classId" required @change="pickClass">
          <option value="">{{ classes.isLoading.value ? "Loading…" : "Choose a class…" }}</option>
          <optgroup v-for="g in concreteGroups" :key="g.area?.id ?? '-'" :label="g.area?.name ?? 'Other'">
            <option v-for="c in g.items" :key="c.id" :value="c.id">{{ c.name }}</option>
          </optgroup>
          <option v-if="cls && !concrete.includes(cls)" :value="cls.id" disabled>{{ cls.name }}{{ cls.isActive ? "" : " (archived)" }}</option>
        </select>
        <span class="hint">The class decides which attributes the CI carries.</span>
      </div>
      <div v-if="closed" class="alert alert-warn" role="alert">
        {{ cls?.name }} is {{ cls?.isAbstract ? "an abstract class: it groups other classes and holds no CIs itself" : "archived: its CIs are kept, but no new ones can be created" }}.
        Choose another class.
      </div>
      <div v-else-if="denied" class="alert alert-error" role="alert">
        None of your permission profiles allows creating {{ cls?.name }} configuration items. Choose another class.
      </div>
      <div v-else-if="classes.data.value?.length && concrete.length === 0" class="alert alert-warn" role="alert">
        None of your permission profiles allows creating configuration items. Ask an administrator for a profile with the
        create right.
      </div>
    </div>
  </section>
  <template v-if="cloneFrom && !denied && !closed && !editor.active">
    <ErrorAlert v-if="source.isError.value" :error="source.error.value" :title="t('clone.loadFailed')" :on-retry="() => source.refetch()" />
    <div v-else-if="sourceOtherClass" class="alert alert-warn" role="alert">{{ t("clone.otherClass", { class: source.data.value?.class.name ?? "" }) }}</div>
    <div v-else-if="clone" class="alert clone-notice" role="status" data-testid="clone-notice">
      <p>
        <strong>{{ t("clone.source") }}</strong>&#32;<RouterLink :to="`/cis/${clone.ci.id}`" dir="auto">{{ clone.ci.label }}</RouterLink>&#32;<span class="mono">({{ clone.ci.ident }})</span>
      </p>
      <p>
        {{ t("clone.notice", { class: cls?.name ?? "", name: clone.ci.label }) }}
        <template v-if="clearedLabels.length > 0">{{ t("clone.noticeFields", { fields: clearedLabels.join(", ") }) }}</template>
      </p>
    </div>
  </template>
  <LoadingState v-if="cloneLoading && !denied && !closed" label="Loading the CI to clone…" />
  <CiForm
    v-else-if="classId && cls && ((!denied && !closed) || editor.active) && (!cloneFrom || !!clone || sourceOtherClass || source.isError.value || editor.active)"
    :key="`${classId}:${clone?.ci.id ?? ''}`"
    mode="create"
    :class-id="classId"
    :class-name="cls.name"
    :editor="editor"
    :clone="clone"
  />
  <ErrorAlert v-if="classId && classes.data.value && !cls" :error="unknownClass" />
</template>

<style scoped>
.clone-notice p {
  margin: 0;
}
.clone-notice p + p {
  margin-top: var(--space-1);
}
</style>
