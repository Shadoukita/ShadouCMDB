<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAttributesOfClasses, useCiClasses, useRecentActivity, type AuditEntry } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { currentLocale, t } from "../../i18n";
import { actionLabel } from "../../lib/auditEvents";
import { formatDate, formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";
import { useClassColours } from "./classColours";

/**
 * The newest changes to CIs across the inventory (design §0 step 12e), from the audit log: the CI, its class,
 * what changed, who changed it and when. The "Recently changed" widget renders this for a caller with
 * audit.view; without it the widget lists the most recently updated CIs instead (CiTableWidget).
 */
const props = defineProps<{ title: string; limit: number }>();
const log = useRecentActivity(() => props.limit);
const colours = useClassColours();
const classes = useCiClasses();
/** Entries written by the seed and imports carry the class id only. */
const className = (v: CiValue, classId: string | undefined) => v.class?.name ?? classes.data.value?.find((c) => c.id === classId)?.name;

interface CiValue {
  label?: string;
  classId?: string;
  class?: { id?: string; name?: string };
  attributes?: Record<string, unknown>;
}
const value = (e: AuditEntry) => ((e.newValue ?? e.oldValue ?? {}) as CiValue);
const classIds = computed(() => (log.data.value?.data ?? []).map((e) => value(e).class?.id ?? value(e).classId).filter((id): id is string => !!id));
const attributes = useAttributesOfClasses(classIds);

/** Changed attributes of an update, by their label; bookkeeping fields are left out. */
function changedFields(e: AuditEntry, classId: string | undefined): string[] {
  const before = ((e.oldValue ?? {}) as CiValue).attributes ?? {};
  const after = ((e.newValue ?? {}) as CiValue).attributes ?? {};
  const defs = classId ? attributes.value?.get(classId) : undefined;
  return [...new Set([...Object.keys(before), ...Object.keys(after)])]
    .filter((k) => JSON.stringify(before[k] ?? null) !== JSON.stringify(after[k] ?? null))
    .map((k) => defs?.find((d) => d.key === k)?.label ?? k);
}

function change(e: AuditEntry, classId: string | undefined): string {
  if (e.action !== "update") return actionLabel(e.action);
  const fields = changedFields(e, classId);
  if (fields.length === 0) return actionLabel(e.action);
  const shown = fields.slice(0, 2).join(", ");
  return fields.length > 2 ? t("dashboard.activity.fieldsMore", { fields: shown, n: fields.length - 2 }) : t("dashboard.activity.fields", { fields: shown });
}

const timeFormat = new Map<string, Intl.DateTimeFormat>();
/** Today's entries by their time, older ones by their date; the tooltip has both. */
function when(iso: string): string {
  const d = new Date(iso);
  if (d.toDateString() !== new Date().toDateString()) return formatDate(iso);
  const lang = currentLocale() === "de" ? "de" : undefined;
  let f = timeFormat.get(lang ?? "");
  if (!f) timeFormat.set(lang ?? "", (f = new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit" })));
  return f.format(d);
}

const rows = computed(() =>
  (log.data.value?.data ?? []).map((e) => {
    const v = value(e);
    const classId = v.class?.id ?? v.classId;
    return {
      e,
      key: `${e.id}`,
      label: v.label ?? e.entityId,
      to: e.action === "delete" ? undefined : `/cis/${e.entityId}`,
      className: className(v, classId),
      colour: classId ? (colours.value.get(classId) ?? 8) : 8,
      change: e.redacted ? actionLabel(e.action) : change(e, classId),
    };
  }),
);
</script>

<template>
  <section class="panel dash-card recent-activity">
    <div class="panel-header">
      <h2>{{ title }}</h2>
      <RouterLink :to="{ path: '/admin/audit', query: { entityType: 'configuration_items' } }">{{ t("dashboard.activity.viewAudit") }}</RouterLink>
    </div>
    <div class="panel-body flush">
      <div v-if="log.isError.value" class="panel-body"><ErrorAlert :error="log.error.value" :on-retry="() => log.refetch()" /></div>
      <p v-else-if="log.data.value && rows.length === 0" class="panel-body muted">{{ t("dashboard.activity.none") }}</p>
      <SkeletonRows v-else-if="log.isPending.value" :label="t('common.loading')" :rows="6" />
      <table v-else class="data activity-table">
        <thead>
          <tr>
            <th scope="col">{{ t("dashboard.col.ci") }}</th>
            <th scope="col">{{ t("dashboard.col.class") }}</th>
            <th scope="col">{{ t("dashboard.col.change") }}</th>
            <th scope="col">{{ t("dashboard.col.by") }}</th>
            <th scope="col" class="num">{{ t("dashboard.col.when") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in rows" :key="r.key">
            <td class="ci-name">
              <RouterLink v-if="r.to" :to="r.to" dir="auto">{{ r.label }}</RouterLink>
              <bdi v-else>{{ r.label }}</bdi>
            </td>
            <td>
              <span v-if="r.className" :class="['class-key', `viz-${r.colour}`]"><span class="key" aria-hidden="true" />{{ r.className }}</span>
            </td>
            <td>{{ r.change }}</td>
            <td><AuditActor :entry="r.e" /></td>
            <td class="num mono"><time :datetime="r.e.occurredAt" :title="formatDateTime(r.e.occurredAt)">{{ when(r.e.occurredAt) }}</time></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
