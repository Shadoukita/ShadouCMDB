<script setup lang="ts">
import { useQueries, useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { useLookupLists, useLookupListValues } from "../../api/datamodel";
import { ciCountQuery, useCiClasses } from "../../api/queries";
import type { UiWidget } from "../../api/uiSettings";
import { t, tAround } from "../../i18n";
import { useSessionStore } from "../../stores/session";
import CountTable, { type CountRow } from "./CountTable.vue";

/** Counts per class, or per value of one lookup list; each count is a server-side limit=1 request. */
const props = defineProps<{ widget: UiWidget; title: string }>();
const session = useSessionStore();
const total = useQuery(ciCountQuery({}));
const classes = useCiClasses();
const lookupLists = useLookupLists();
const list = computed(() =>
  props.widget.type === "count_by_lookup" ? lookupLists.data.value?.find((l) => l.key === props.widget.lookupListKey) : undefined,
);
const values = useLookupListValues(() => list.value?.id);
/** Without a title of its own, a lookup widget is named after its list. */
const heading = computed(() => props.widget.title || (list.value ? t("dashboard.byList", { list: list.value.name }) : props.title));

interface Row {
  id: string;
  label: string;
  query: Record<string, string>;
  to: string;
  newTo?: string;
  newLabel?: string;
}
const rows = computed<Row[]>(() => {
  if (props.widget.type !== "count_by_class") {
    return list.value
      ? (values.data.value ?? []).map((v) => ({ id: v.id, label: v.name, query: { lookupValueId: v.id }, to: `/cis?lookupValueId=${v.id}` }))
      : [];
  }
  const keys = props.widget.classKeys ?? [];
  // Only classes the user may view: the API leaves the others out of every count, which would read as 0.
  const visible = (classes.data.value ?? []).filter(
    (c) => !c.isAbstract && session.canOnClass(c.id, "view") && (keys.length ? keys.includes(c.key) : true),
  );
  return visible.map((c) => ({
    id: c.id,
    label: c.name,
    query: { classId: c.id },
    to: `/cis?classId=${c.id}`,
    ...(c.isActive && session.canOnClass(c.id, "create") ? { newTo: `/cis/new?classId=${c.id}`, newLabel: t("dashboard.newIn", { class: c.name }) } : {}),
  }));
});
const loading = computed(() =>
  props.widget.type === "count_by_class" ? classes.isLoading.value : lookupLists.isLoading.value || values.isLoading.value,
);
const sourceError = computed(() => (props.widget.type === "count_by_class" ? classes.error.value : (lookupLists.error.value ?? values.error.value)));
const counts = useQueries({ queries: computed(() => rows.value.map((r) => ciCountQuery(r.query))) });
const listMissing = computed(() => tAround("dashboard.listMissing", "key"));
const countRows = computed<CountRow[]>(() => rows.value.map((r, i) => ({ ...r, count: counts.value[i]?.data })));
</script>

<template>
  <CountTable
    :title="heading"
    :rows="countRows"
    :total="total.data.value ?? 0"
    :loading="loading"
    :error="sourceError ?? counts.find((c) => c.error)?.error"
  />
  <p v-if="widget.type === 'count_by_lookup' && lookupLists.data.value && !list" class="muted">
    {{ listMissing[0] }}<code>{{ widget.lookupListKey }}</code>{{ listMissing[1] }}
  </p>
</template>
