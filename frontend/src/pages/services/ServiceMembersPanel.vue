<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useServiceMembers, type Service, type ServiceMemberQuery } from "../../api/services";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import EmptyState from "../../components/EmptyState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { formatDateTime } from "../../lib/format";
import ServiceError from "./ServiceError.vue";

/**
 * The Members tab (spec §5.3): the direct members the caller may view, paged on the server. Its page,
 * page size and sort live in the URL (`mpage`, `mlimit`, `msort`) next to `tab=members`, so the view survives a reload.
 */
const props = defineProps<{ service: Service }>();
const route = useRoute();
const router = useRouter();
const DEFAULT_LIMIT = 50;
const SORTS = ["name", "class", "criticality", "addedAt"] as const;

const one = (k: string) => (typeof route.query[k] === "string" ? (route.query[k] as string) : "");
const page = computed(() => (/^\d+$/.test(one("mpage")) ? Math.max(1, Number(one("mpage"))) : 1));
const sort = computed(() => (SORTS.includes(one("msort").replace(/^-/, "") as (typeof SORTS)[number]) ? one("msort") : "name"));
const limit = computed(() => (/^\d+$/.test(one("mlimit")) ? Math.min(200, Math.max(1, Number(one("mlimit")))) : DEFAULT_LIMIT));
const query = computed<ServiceMemberQuery>(() => ({ limit: limit.value, offset: (page.value - 1) * limit.value, sort: sort.value as ServiceMemberQuery["sort"] }));
const members = useServiceMembers(() => props.service.id, query);
const rows = computed(() => members.data.value?.data ?? []);
const total = computed(() => members.data.value?.page.total ?? 0);

// The only query keys this tab keeps; anything else in the URL is dropped rather than copied over.
const QUERY_KEYS = ["tab", "mpage", "mlimit", "msort"] as const;
type QueryKey = (typeof QUERY_KEYS)[number];

function setQuery(patch: Partial<Record<QueryKey, string | undefined>>) {
  const next = new Map<QueryKey, string>();
  for (const k of QUERY_KEYS) {
    const v = k in patch ? patch[k] : one(k);
    if (v) next.set(k, v);
  }
  void router.push({ path: route.path, query: Object.fromEntries(next) });
}
const toggleSort = (f: string) => setQuery({ msort: sort.value === f ? `-${f}` : f === "name" ? undefined : f, mpage: undefined });
const ariaSort = (f: string) => (sort.value === f ? "ascending" : sort.value === `-${f}` ? "descending" : "none");
const indicator = (f: string) => (sort.value === f ? "▲" : sort.value === `-${f}` ? "▼" : "");
const onPage = (p: { limit: number; offset: number }) =>
  setQuery({ mpage: p.offset > 0 ? String(Math.floor(p.offset / p.limit) + 1) : undefined, mlimit: p.limit === DEFAULT_LIMIT ? undefined : String(p.limit) });
const COLUMNS: [string, string | null][] = [
  ["services.members.col.name", "name"],
  ["services.members.col.class", "class"],
  ["services.col.criticality", "criticality"],
  ["services.members.col.kind", null],
  ["services.col.active", null],
  ["services.members.col.added", "addedAt"],
];
</script>

<template>
  <section class="panel" :aria-label="t('services.members.title')">
    <p v-if="service.visibility === 'restricted'" class="note" role="note">{{ t("services.members.restricted") }}</p>
    <ServiceError v-if="members.isError.value" :error="members.error.value" :on-retry="() => members.refetch()" />
    <div v-else-if="members.isPending.value" class="table-wrap" aria-busy="true">
      <span class="sr-only" role="status">{{ t("services.members.loading") }}</span>
      <div class="skeleton-table" aria-hidden="true"><div v-for="n in 5" :key="n" class="skeleton-row" /></div>
    </div>
    <EmptyState v-else-if="total === 0" :title="t('services.members.empty')" />
    <template v-else>
      <div class="table-wrap">
        <table :class="['data', { loading: members.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="[label, f] in COLUMNS" :key="label" scope="col" :aria-sort="f ? ariaSort(f) : undefined">
                <button v-if="f" type="button" class="sort" @click="toggleSort(f)">{{ t(label as "services.col.criticality") }} {{ indicator(f) }}</button>
                <template v-else>{{ t(label as "services.col.criticality") }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="m in rows" :key="m.membershipId">
              <td>
                <RouterLink :to="m.isService ? `/services/${m.ci.id}` : `/cis/${m.ci.id}`" dir="auto">{{ m.ci.name }}</RouterLink>
                <span class="muted mono"> {{ m.ci.ident }}</span>
              </td>
              <td dir="auto">{{ m.ci.className }}</td>
              <td><CriticalityBadge :value="m.ci.criticality" show-unset /></td>
              <td>{{ m.isService ? t("services.members.kind.serviceOne") : t("services.members.kind.ciOne") }}</td>
              <td>{{ m.ci.active ? t("common.yes") : t("common.no") }}</td>
              <td>{{ formatDateTime(m.addedAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="(page - 1) * limit" @change="onPage" />
    </template>
  </section>
</template>
