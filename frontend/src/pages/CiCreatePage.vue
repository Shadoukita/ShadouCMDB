<script setup lang="ts">
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useCiClasses } from "../api/queries";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import { useDocumentTitle } from "../lib/composables";
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
const denied = computed(() => !!cls.value && !session.canOnClass(cls.value.id, "create"));
const crumbs = computed<Crumb[]>(() => [
  { label: "Inventory", to: "/cis" },
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
  <section class="panel">
    <div class="panel-body">
      <div class="field" style="max-width: 320px">
        <label for="ci-class">Class<span class="req" aria-hidden="true">*</span></label>
        <ErrorAlert v-if="classes.isError.value" :error="classes.error.value" :on-retry="() => classes.refetch()" />
        <select v-else id="ci-class" v-autofocus="!classId" :value="classId" required @change="pickClass">
          <option value="">{{ classes.isLoading.value ? "Loading…" : "Choose a class…" }}</option>
          <option v-for="c in concrete" :key="c.id" :value="c.id">{{ c.name }}</option>
        </select>
        <span class="hint">The class decides which attributes the CI carries.</span>
      </div>
    </div>
  </section>
  <div v-if="denied" class="alert alert-error" role="alert">
    None of your permission profiles allows creating {{ cls?.name }} configuration items. Choose another class.
  </div>
  <div v-else-if="classes.data.value && concrete.length === 0" class="alert alert-warn" role="alert">
    None of your permission profiles allows creating configuration items. Ask an administrator for a profile with the
    create right.
  </div>
  <CiForm v-if="classId && cls && !denied" :key="classId" mode="create" :class-id="classId" :class-name="cls.name" />
  <ErrorAlert v-if="classId && classes.data.value && !cls" :error="unknownClass" />
</template>
