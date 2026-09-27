<script setup lang="ts">
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useAreas } from "../api/datamodel";
import { useCiClasses } from "../api/queries";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import { useDocumentTitle } from "../lib/composables";
import { groupByArea } from "../lib/areas";
import { vAutofocus } from "../lib/directives";
import { useSessionStore } from "../stores/session";
import CiForm from "./form/CiForm.vue";

const route = useRoute();
const router = useRouter();
const classId = computed(() => (typeof route.query.classId === "string" ? route.query.classId : ""));
const classes = useCiClasses();
const cls = computed(() => classes.data.value?.find((c) => c.id === classId.value));
useDocumentTitle(() => (cls.value ? `New ${cls.value.name}` : "New CI"));

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
const crumbs = computed<Crumb[]>(() => [
  { label: "Inventory", to: "/cis" },
  ...(area.value ? [{ label: area.value.name }] : []),
  ...(cls.value ? [{ label: cls.value.name, to: `/cis?classId=${cls.value.id}` }] : []),
  { label: "New" },
]);

const unknownClass = computed(() => new Error(`Class ${classId.value} does not exist.`));

function pickClass(e: Event) {
  const value = (e.target as HTMLSelectElement).value;
  router.replace({ path: "/cis/new", query: value ? { classId: value } : {} });
}
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <div class="page-header">
    <div class="title"><h1>New configuration item</h1></div>
  </div>
  <section v-if="classes.data.value?.length === 0" class="panel callout">
    <DataModelEmpty />
  </section>
  <section v-else class="panel">
    <div class="panel-body">
      <div class="field" style="max-width: 320px">
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
    </div>
  </section>
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
  <CiForm v-if="classId && cls && !denied && !closed" :key="classId" mode="create" :class-id="classId" :class-name="cls.name" />
  <ErrorAlert v-if="classId && classes.data.value && !cls" :error="unknownClass" />
</template>
