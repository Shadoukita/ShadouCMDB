<script setup lang="ts">
import { useQueries, useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { ciCountQuery, useCiClasses, useLookup } from "../../api/queries";
import type { UiWidget } from "../../api/uiSettings";
import { useSessionStore } from "../../stores/session";
import CountTable, { type CountRow } from "./CountTable.vue";

/** Counts per class, status or environment; each count is a server-side limit=1 request. */
const props = defineProps<{ widget: UiWidget; title: string }>();
const session = useSessionStore();
const total = useQuery(ciCountQuery({}));
const classes = useCiClasses();
const statuses = useLookup("statuses");
const environments = useLookup("environments");

interface Row {
  id: string;
  label: string;
  query: Record<string, string>;
  to: string;
  newTo?: string;
  newLabel?: string;
}
const rows = computed<Row[]>(() => {
  switch (props.widget.type) {
    case "count_by_class": {
      const keys = props.widget.classKeys ?? [];
      const list = (classes.data.value ?? []).filter((c) => !c.isAbstract && (keys.length ? keys.includes(c.key) : true));
      return list.map((c) => ({
        id: c.id,
        label: c.name,
        query: { classId: c.id },
        to: `/cis?classId=${c.id}`,
        ...(c.isActive && session.canOnClass(c.id, "create") ? { newTo: `/cis/new?classId=${c.id}`, newLabel: `New ${c.name}` } : {}),
      }));
    }
    case "count_by_status":
      return (statuses.data.value ?? []).map((s) => ({ id: s.id, label: s.name, query: { statusId: s.id }, to: `/cis?statusId=${s.id}` }));
    default:
      return (environments.data.value ?? []).map((e) => ({ id: e.id, label: e.name, query: { environmentId: e.id }, to: `/cis?environmentId=${e.id}` }));
  }
});
const source = computed(() => (props.widget.type === "count_by_class" ? classes : props.widget.type === "count_by_status" ? statuses : environments));
const counts = useQueries({ queries: computed(() => rows.value.map((r) => ciCountQuery(r.query))) });
const countRows = computed<CountRow[]>(() => rows.value.map((r, i) => ({ ...r, count: counts.value[i]?.data })));
</script>

<template>
  <CountTable
    :title="title"
    :rows="countRows"
    :total="total.data.value ?? 0"
    :loading="source.isLoading.value"
    :error="source.error.value ?? counts.find((c) => c.error)?.error"
  />
</template>
