<script setup lang="ts">
import { computed } from "vue";
import { useClassAttributes, type Ci } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { groupAttributes } from "../../lib/attributes";
import type { TrailStep } from "../../lib/trail";
import AttributeValue from "./AttributeValue.vue";

const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[] }>();
const attrs = useClassAttributes(() => props.ci.classId);
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || values.value[d.key] != null));
const groups = computed(() => groupAttributes(defs.value));
/** Values stored on the CI that no current definition describes (e.g. after a class change). */
const orphans = computed(() => {
  const known = new Set(defs.value.map((d) => d.key));
  return Object.keys(values.value).filter((k) => !known.has(k));
});
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>{{ ci.class.name }} attributes</h2></div>
    <div class="panel-body">
      <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
      <ErrorAlert
        v-if="attrs.isError.value"
        :error="attrs.error.value"
        title="Could not load attribute definitions"
        :on-retry="() => attrs.refetch()"
      />
      <p v-if="attrs.data.value && defs.length === 0 && orphans.length === 0" class="muted">This class defines no extra attributes.</p>
      <dl v-if="attrs.data.value && (defs.length > 0 || orphans.length > 0)" class="props">
        <template v-for="[g, items] in groups" :key="g">
          <div class="group-title">{{ g }}</div>
          <template v-for="d in items" :key="d.key">
            <dt>{{ d.label }}</dt>
            <dd><AttributeValue :def="d" :value="values[d.key]" :ref-info="refs[d.key]" :self="self" :trail="trail" /></dd>
          </template>
        </template>
        <template v-if="orphans.length > 0">
          <div class="group-title">Not defined by this class</div>
          <template v-for="k in orphans" :key="k">
            <dt>{{ k }}</dt>
            <dd><span class="mono">{{ JSON.stringify(values[k]) }}</span></dd>
          </template>
        </template>
      </dl>
    </div>
  </section>
</template>
