<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAuditLog, useClassAttributes, type AuditEntry, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { actionLabel, actionTone, eventSource, formatUtc, sourceLabel } from "../../lib/auditEvents";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";
import ChangeValue from "../imports/ChangeValue.vue";

/**
 * The CI's history as an event stream (design §2.7, audit R8): GET /audit-log?entityId=…, newest first and
 * paged on the server, with the time in UTC, the actor, the source the change came through, the event and
 * a field-level diff of each update. `embedded`: placed in a layout section, which gives the frame and the heading.
 */
const props = defineProps<{ ci: Ci; embedded?: boolean }>();

/**
 * Bookkeeping and derived fields (the label follows the title attribute, `active` the validity period).
 * Entries from before CIs kept status, owner etc. in attributes still carry those ids and references.
 */
const HIDDEN = new Set(["updatedAt", "createdAt", "version", "classId", "label", "active", "attributeReferences", "statusId", "environmentId", "ownerId", "locationId"]);
/** Embedded references are shown by name instead of by id. */
const REFS = new Set(["class", "status", "environment", "owner", "location"]);

const paging = ref({ limit: 50, offset: 0 });
watch(
  () => props.ci.id,
  () => (paging.value = { limit: paging.value.limit, offset: 0 }),
);
const log = useAuditLog(() => props.ci.id, paging);
const entries = computed(() => log.data.value?.data ?? []);
const total = computed(() => log.data.value?.page.total ?? 0);
/** Lookup and reference fields store ids: their definitions (retired ones too) let ChangeValue show names. */
const attrs = useClassAttributes(() => props.ci.classId, { includeInactive: true });
const fieldDef = (key: string) => attrs.data.value?.find((a) => a.key === key);

function changes(entry: AuditEntry) {
  const oldV = flatten(entry.oldValue);
  const newV = flatten(entry.newValue);
  const keys = [...new Set([...Object.keys(oldV), ...Object.keys(newV)])].filter(
    (k) => !HIDDEN.has(k.split(".")[0]) && oldV[k] !== newV[k],
  );
  return keys.map((k) => ({ key: k, def: k.startsWith("attributes.") ? fieldDef(k.slice("attributes.".length)) : undefined, old: oldV[k], new: newV[k] }));
}

function flatten(value: unknown, prefix = ""): Record<string, string> {
  const out: Record<string, string> = {};
  if (!value || typeof value !== "object") return out;
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v === null || v === undefined) continue;
    if (REFS.has(k) && typeof v === "object" && "name" in v) {
      out[key] = String((v as { name: unknown }).name);
      continue;
    }
    if (typeof v === "object" && !Array.isArray(v) && k === "attributes") Object.assign(out, flatten(v, key));
    else out[key] = typeof v === "object" ? JSON.stringify(v) : String(v);
  }
  return out;
}

/** Workflow steps on the CI (`workflow.*`): their details are in the values, not a field diff. */
type WorkflowValue = { instanceId?: string; definitionKey?: string; transitionKey?: string; stateKey?: string; comment?: string | null; reason?: string; fields?: Record<string, { old?: unknown; new?: unknown }> };
function workflowStep(entry: AuditEntry) {
  const o = (entry.oldValue ?? {}) as WorkflowValue;
  const n = (entry.newValue ?? {}) as WorkflowValue;
  const show = (v: unknown) => (v === null || v === undefined ? undefined : typeof v === "object" ? JSON.stringify(v) : String(v));
  return {
    instanceId: n.instanceId ?? o.instanceId,
    definitionKey: n.definitionKey ?? o.definitionKey ?? "",
    transitionKey: n.transitionKey,
    from: entry.action === "workflow.cancel" ? undefined : o.stateKey,
    to: n.stateKey,
    note: n.reason === "ci_deleted" ? t("history.wf.ciDeleted") : (n.comment ?? n.reason ?? undefined),
    fields: Object.entries(n.fields ?? {}).map(([k, v]) => ({ key: k, def: fieldDef(k), old: show(v?.old), new: show(v?.new) })),
  };
}

</script>

<template>
  <LoadingState v-if="log.isLoading.value" :label="t('history.loading')" />
  <ErrorAlert v-else-if="log.isError.value && !log.data.value" :error="log.error.value" :on-retry="() => log.refetch()" />
  <EmptyState v-else-if="total === 0" :title="t('history.empty.title')">{{ t("history.empty.body") }}</EmptyState>
  <section v-else :class="['event-stream', { panel: !embedded }]">
    <div class="event-stream-head muted">{{ t("history.order") }}</div>
    <ErrorAlert v-if="log.isError.value" :error="log.error.value" :on-retry="() => log.refetch()" />
    <div class="table-wrap">
      <table class="data event-table" :aria-busy="log.isFetching.value || undefined">
        <thead>
          <tr>
            <th scope="col">{{ t("history.col.time") }}</th>
            <th scope="col">{{ t("history.col.actor") }}</th>
            <th scope="col">{{ t("history.col.source") }}</th>
            <th scope="col">{{ t("history.col.event") }}</th>
            <th scope="col" class="event-change">{{ t("history.col.change") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="e.id">
            <td class="mono event-time"><time :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatUtc(e.occurredAt) }}</time></td>
            <td><AuditActor :entry="e" /></td>
            <td><span class="chip event-source">{{ sourceLabel(eventSource(e)) }}</span></td>
            <td>
              <span :class="['badge', actionTone(e.action)]" :title="e.action">{{ actionLabel(e.action) }}</span>
            </td>
            <td class="event-change">
              <template v-if="e.action.startsWith('workflow.')">
                <div v-for="w in [workflowStep(e)]" :key="e.id" data-testid="history-workflow">
                  <RouterLink v-if="w.instanceId" :to="`/workflows/${w.instanceId}`" class="mono">{{ w.definitionKey }}</RouterLink>
                  <span v-else class="mono">{{ w.definitionKey }}</span>
                  <template v-if="w.transitionKey">: <code>{{ w.transitionKey }}</code></template>
                  <span v-if="w.to">
                    ,
                    <template v-if="w.from">
                      <span class="sr-only">{{ t("history.diff.from") }}</span><code>{{ w.from }}</code>
                      <Icon name="arrow-right" :size="12" class="diff-arrow" />
                    </template>
                    <span class="sr-only">{{ t("history.diff.to") }}</span><code>{{ w.to }}</code>
                  </span>
                  <div v-if="w.note" class="wf-comment muted" dir="auto">{{ w.note }}</div>
                  <ul v-if="w.fields.length > 0" class="diff">
                    <li v-for="c in w.fields" :key="c.key">
                      <code>{{ c.key }}</code>
                      <span class="sr-only">{{ t("history.diff.from") }}</span>
                      <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del>
                      <Icon name="arrow-right" :size="12" class="diff-arrow" />
                      <span class="sr-only">{{ t("history.diff.to") }}</span>
                      <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">{{ t("history.diff.cleared") }}</span>
                    </li>
                  </ul>
                </div>
              </template>
              <span v-else-if="e.action === 'create'" class="muted">{{ t("history.created") }}</span>
              <span v-else-if="e.action === 'delete'" class="muted">{{ t("history.deleted") }}</span>
              <span v-else-if="e.redacted" class="muted">{{ t("history.redacted") }}</span>
              <span v-else-if="changes(e).length === 0" class="muted">{{ t("history.noChanges") }}</span>
              <ul v-else class="diff">
                <li v-for="c in changes(e)" :key="c.key">
                  <code>{{ c.key }}</code>
                  <span class="sr-only">{{ t("history.diff.from") }}</span>
                  <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del>
                  <Icon name="arrow-right" :size="12" class="diff-arrow" />
                  <span class="sr-only">{{ t("history.diff.to") }}</span>
                  <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">{{ t("history.diff.cleared") }}</span>
                </li>
              </ul>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <PaginationBar v-if="total > paging.limit" :total="total" :limit="paging.limit" :offset="paging.offset" @change="(p) => (paging = p)" />
  </section>
</template>
