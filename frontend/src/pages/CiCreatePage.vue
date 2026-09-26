<script setup lang="ts">
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useCiClasses } from "../api/queries";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import { useDocumentTitle } from "../lib/composables";
import { vAutofocus } from "../lib/directives";
import CiForm from "./form/CiForm.vue";

const route = useRoute();
const router = useRouter();
const classId = computed(() => (typeof route.query.classId === "string" ? route.query.classId : ""));
const classes = useCiClasses();
const cls = computed(() => classes.data.value?.find((c) => c.id === classId.value));
useDocumentTitle(() => (cls.value ? `New ${cls.value.name}` : "New CI"));

const concrete = computed(() => (classes.data.value ?? []).filter((c) => c.isActive && !c.isAbstract));
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
  <CiForm v-if="classId && cls" :key="classId" mode="create" :class-id="classId" :class-name="cls.name" />
  <ErrorAlert v-if="classId && classes.data.value && !cls" :error="unknownClass" />
</template>
