<script setup lang="ts">
import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { api, unwrap } from "../../api/client";
import { useAllLookupListValues, useLookupLists } from "../../api/datamodel";
import { keys, useCiClasses, type CiListQuery, type CiSummary } from "../../api/queries";
import type { UiListFilters, UiWidget } from "../../api/uiSettings";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import { formatRelative } from "../../lib/format";
import { attributeKey, compareValues, lookupValueIds, sortParam } from "../../lib/uiSettings";

/**
 * "Recently changed" and saved searches: the first `limit` CIs of a server-side
 * list. The list API filters by one class, so a search over several classes
 * asks once per class and merges the first rows of each (their union's first
 * rows are among them); the total is the sum.
 */
const props = defineProps<{ widget: UiWidget; title: string }>();
const limit = computed(() => props.widget.limit ?? 10);
const classes = useCiClasses();
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();

const search = computed(() => (props.widget.type === "saved_search" ? props.widget.search : undefined));
const sort = computed(() => {
  if (!search.value) return "-updatedAt";
  const p = sortParam(search.value.sort) ?? "label";
  // An attribute sort needs a class; without one (the classes were unticked) the list API would refuse it.
  return attributeKey(p.replace(/^-/, "")) !== null && !search.value.classKeys?.length ? "label" : p;
});
const ready = computed(
  () => !search.value || (!!classes.data.value && !!lookupLists.data.value && !!lookupValues.data.value),
);

const queries = computed<CiListQuery[]>(() => {
  const s = search.value;
  const base: CiListQuery = { sort: sort.value, limit: limit.value };
  if (!s) return [base];
  const f: UiListFilters = s.filters ?? { q: null };
  Object.assign(base, {
    q: f.q || undefined,
    lookupValueId: lookupValueIds(f.lookups, lookupLists.data.value ?? [], lookupValues.data.value ?? []),
  });
  const classIds = (classes.data.value ?? []).filter((c) => s.classKeys?.includes(c.key)).map((c) => c.id);
  if (classIds.length === 0) return [base];
  return classIds.map((classId) => ({ ...base, classId, includeSubclasses: s.includeSubclasses ? "true" : "false" }));
});
const results = useQueries({
  queries: computed(() =>
    queries.value.map((q) => ({
      queryKey: keys.ciList(q),
      enabled: ready.value,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items", { params: { query: q }, signal })),
    })),
  ),
});
const loading = computed(() => !ready.value || results.value.some((r) => r.isLoading));
const error = computed(() => results.value.find((r) => r.error)?.error);
const total = computed(() => results.value.reduce((n, r) => n + (r.data?.page.total ?? 0), 0));
const rows = computed<CiSummary[]>(() => {
  const all = results.value.flatMap((r) => r.data?.data ?? []);
  if (results.value.length > 1) {
    const field = sort.value.replace(/^-/, "");
    const dir = sort.value.startsWith("-") ? -1 : 1;
    const attr = attributeKey(field);
    const val = (c: CiSummary): unknown =>
      attr !== null ? (c as CiSummary & { attributes?: Record<string, unknown> }).attributes?.[attr] : field === "className" ? c.class.name : c[field as keyof CiSummary];
    all.sort((a, b) => compareValues(val(a), val(b), dir));
  }
  return all.slice(0, limit.value);
});
/** "View all" opens the inventory with the same filters (one class at most, as the inventory filters by one). */
const viewAll = computed(() => {
  const q = queries.value.length === 1 ? queries.value[0] : null;
  if (!q) return null;
  const out: Record<string, string> = {};
  for (const k of ["q", "classId", "lookupValueId", "sort"] as const) if (q[k]) out[k] = String(q[k]);
  return { path: "/cis", query: out };
});
</script>

<template>
  <section class="panel">
    <div class="panel-header">
      <h2>{{ title }} <span v-if="search && !loading && !error" class="muted">{{ total.toLocaleString() }}</span></h2>
      <RouterLink v-if="viewAll" :to="viewAll">View all</RouterLink>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="loading" />
      <div v-if="error" class="panel-body"><ErrorAlert :error="error" :on-retry="() => results.forEach((r) => r.refetch())" /></div>
      <p v-else-if="!loading && rows.length === 0" class="panel-body muted">No configuration items match.</p>
      <table v-if="!loading && rows.length > 0" class="data">
        <thead>
          <tr>
            <th scope="col">Label</th>
            <th scope="col">Ident</th>
            <th scope="col">Class</th>
            <th scope="col">Changed</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="ci in rows" :key="ci.id">
            <td><RouterLink :to="`/cis/${ci.id}`">{{ ci.label }}</RouterLink> <CiStateBadge :ci="ci" /></td>
            <td class="mono">{{ ci.ident }}</td>
            <td>{{ ci.class.name }}</td>
            <td :title="ci.updatedAt">{{ formatRelative(ci.updatedAt) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
