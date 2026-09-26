<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";

export interface CountRow {
  id: string;
  label: string;
  count: number | undefined;
  to: string;
  /** Optional "+ New" link per row. */
  newTo?: string;
  newLabel?: string;
}

const props = defineProps<{ title: string; rows: CountRow[]; total: number; loading: boolean; error: unknown }>();
const sorted = computed(() =>
  [...props.rows].sort((a, b) => (b.count ?? -1) - (a.count ?? -1) || a.label.localeCompare(b.label)),
);
const hasExtra = computed(() => props.rows.some((r) => r.newTo));
const barWidth = (count: number | undefined) =>
  `${props.total && count ? Math.max(1, (count / props.total) * 100) : 0}%`;
</script>

<template>
  <section class="panel">
    <div class="panel-header">
      <h2>{{ title }}</h2>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="loading" />
      <div v-if="error != null" class="panel-body">
        <ErrorAlert :error="error" title="Some counts could not be loaded" />
      </div>
      <table v-if="!loading" class="data">
        <tbody>
          <tr v-for="r in sorted" :key="r.id">
            <td style="width: 35%"><RouterLink :to="r.to">{{ r.label }}</RouterLink></td>
            <td class="num" style="width: 70px">
              <span v-if="r.count === undefined" class="spinner" aria-label="Loading" />
              <template v-else>{{ r.count.toLocaleString() }}</template>
            </td>
            <td><div class="bar" :style="{ width: barWidth(r.count) }" aria-hidden="true" /></td>
            <td v-if="hasExtra" class="num">
              <RouterLink v-if="r.newTo" :to="r.newTo" :aria-label="r.newLabel">+ New</RouterLink>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
