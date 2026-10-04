<script setup lang="ts">
import { computed } from "vue";
import { useRecentChanges, useRelationships, type Ci } from "../../api/queries";
import { useServiceSettings, useServicesOfCi } from "../../api/services";
import { t } from "../../i18n";
import { formatRelative } from "../../lib/format";
import { describeEdge } from "../../lib/relationships";
import { useSessionStore } from "../../stores/session";

/**
 * The stat tiles under a CI's title (design §2.7, record page): its relationships, the business services it
 * is part of, and its changes in the last 30 days. A tile shows only what is known: one the caller may not
 * see (no audit.view, no business services) or whose request failed is left out, never shown as 0.
 */
const props = defineProps<{ ci: Ci }>();
const DAYS = 30;
const session = useSessionStore();
const live = computed(() => !props.ci.deletedAt);

const rels = useRelationships(() => props.ci.id);
const relTotal = computed(() => rels.data.value?.page.total);
/** Outgoing and incoming, when the whole list was fetched. */
const relSplit = computed(() => {
  const d = rels.data.value;
  if (!d || d.data.length < d.page.total) return null;
  const out = d.data.filter((r) => describeEdge(r, props.ci.id).outgoing).length;
  return { out, in: d.data.length - out };
});

const serviceSettings = useServiceSettings();
const canViewServices = computed(() => live.value && !!serviceSettings.data.value?.canView);
const services = useServicesOfCi(() => props.ci.id, canViewServices);

const canAudit = computed(() => session.can("audit.view"));
const changes = useRecentChanges(() => props.ci.id, DAYS, canAudit);
const last = computed(() => changes.data.value?.data[0]);

interface Tile {
  key: string;
  label: string;
  value?: string;
  note?: string;
  pending: boolean;
}
const tiles = computed<Tile[]>(() => {
  const out: Tile[] = [];
  if (!rels.isError.value) {
    out.push({
      key: "relationships",
      label: t("record.stat.relationships"),
      value: relTotal.value?.toLocaleString(),
      note: relSplit.value ? t("record.stat.relationships.note", { out: relSplit.value.out, in: relSplit.value.in }) : undefined,
      pending: rels.isPending.value,
    });
  }
  if (canViewServices.value && !services.isError.value) {
    out.push({
      key: "services",
      label: t("record.stat.services"),
      value: services.data.value?.data.length.toLocaleString(),
      note: t("record.stat.services.note"),
      pending: services.isPending.value,
    });
  }
  if (canAudit.value && !changes.isError.value) {
    const l = last.value;
    out.push({
      key: "changes",
      label: t("record.stat.changes"),
      value: changes.data.value?.page.total.toLocaleString(),
      note: !l
        ? t("record.stat.changes.none")
        : l.actorName
          ? t("record.stat.changes.noteBy", { when: formatRelative(l.occurredAt), actor: l.actorName })
          : t("record.stat.changes.note", { when: formatRelative(l.occurredAt) }),
      pending: changes.isPending.value,
    });
  }
  return out;
});
</script>

<template>
  <section v-if="tiles.length > 0" class="record-stats" :aria-label="t('record.stats')" data-testid="record-stats">
    <div v-for="tile in tiles" :key="tile.key" class="stat" :data-stat="tile.key" :aria-busy="tile.pending">
      <span class="value">
        <span v-if="tile.pending" class="skeleton-line" aria-hidden="true" />
        <template v-else>{{ tile.value }}</template>
      </span>
      <span class="label">{{ tile.label }}</span>
      <span v-if="!tile.pending && tile.note" class="note" dir="auto">{{ tile.note }}</span>
    </div>
  </section>
</template>
