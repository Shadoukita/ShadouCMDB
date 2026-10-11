<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useAuditLog, useClassAttributes, type AuditEntry, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { actionLabel, actionTone, EVENT_SOURCES, eventSource, formatUtc, sourceLabel, type EventSource } from "../../lib/auditEvents";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";
import ChangeValue from "../imports/ChangeValue.vue";
import HistoryPointInTime from "./HistoryPointInTime.vue";
import HistoryWorkflowStep from "./HistoryWorkflowStep.vue";

/**
 * The CI's history as a timeline (design §2.7 and §0 step 12d, audit R8): GET /audit-log?entityId=…, newest
 * first and paged on the server. Each entry is a dot in the event's tone, the actor and the event, the source
 * the change came through, the field-level diff of an update as old → new chips, and the time in UTC.
 * Source chips narrow it on the server. `embedded`: placed in a layout section, which gives the frame and the
 * heading. `preview`: the newest `preview` entries only, under their own heading, with no source filter or
 * paging; "Full history" (`more`) opens the History tab. Outside the preview the timeline switches to the CI
 * as of a past moment or to the difference between two moments (SHAA-3131), which a timeline entry can set.
 */
const props = defineProps<{ ci: Ci; embedded?: boolean; preview?: number }>();
const emit = defineEmits<{ more: [] }>();

/**
 * Bookkeeping and derived fields (the label follows the title attribute, `active` the validity period).
 * Entries from before CIs kept status, owner etc. in attributes still carry those ids and references.
 */
const HIDDEN = new Set(["updatedAt", "createdAt", "version", "classId", "label", "active", "attributeReferences", "statusId", "environmentId", "ownerId", "locationId"]);
/** Embedded references are shown by name instead of by id. */
const REFS = new Set(["class", "status", "environment", "owner", "location"]);

const paging = ref({ limit: props.preview ?? 50, offset: 0 });
/** Sources shown; none selected shows every source. */
const sources = ref<EventSource[]>([]);
const actorTypes = computed(() => EVENT_SOURCES.filter((s) => sources.value.includes(s.source)).map((s) => s.actorType));
watch(
  () => props.ci.id,
  () => (sources.value = []),
);
watch([() => props.ci.id, sources], () => (paging.value = { limit: paging.value.limit, offset: 0 }));
function toggleSource(source: EventSource) {
  sources.value = sources.value.includes(source) ? sources.value.filter((s) => s !== source) : [...sources.value, source];
}
const log = useAuditLog(() => props.ci.id, paging, actorTypes);
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

/** Timeline, the CI as of a moment, or the difference between two moments; the moments are ISO 8601, in UTC. */
type View = "timeline" | "asOf" | "compare";
const VIEWS: readonly View[] = ["timeline", "asOf", "compare"];
const viewLabel = (v: View) => t(`history.view.${v}`);
const view = ref<View>("timeline");
const startOfDayUtc = (daysAgo: number) => new Date(Math.floor(Date.now() / 86_400_000 - daysAgo) * 86_400_000).toISOString();
const asOf = ref(startOfDayUtc(7));
const compareFrom = ref(startOfDayUtc(7));
const compareTo = ref(new Date().toISOString());
/** Timeline entries picked for a comparison (at most two). */
const picked = ref<AuditEntry[]>([]);
watch(
  () => props.ci.id,
  () => {
    view.value = "timeline";
    picked.value = [];
  },
);
function viewAsOf(e: AuditEntry) {
  asOf.value = e.occurredAt;
  view.value = "asOf";
}
const isPicked = (e: AuditEntry) => picked.value.some((p) => p.id === e.id);
function togglePick(e: AuditEntry) {
  picked.value = isPicked(e) ? picked.value.filter((p) => p.id !== e.id) : [...picked.value, e].slice(-2);
}
/** From just before the older entry, so its change is part of the difference, to just after the newer one. */
function comparePicked() {
  const [a, b] = [...picked.value].sort((x, y) => Date.parse(x.occurredAt) - Date.parse(y.occurredAt));
  compareFrom.value = new Date(Date.parse(a.occurredAt) - 1).toISOString();
  compareTo.value = b.occurredAt;
  view.value = "compare";
}

/** The dot's colour: the event's tone, a workflow step purple, a plain update the primary colour. */
const dotTone = (e: AuditEntry) => actionTone(e.action) || "update";
</script>

<template>
  <LoadingState v-if="log.isLoading.value" :label="t('history.loading')" />
  <ErrorAlert v-else-if="log.isError.value && !log.data.value" :error="log.error.value" :on-retry="() => log.refetch()" />
  <EmptyState v-else-if="total === 0 && sources.length === 0 && !preview" :title="t('history.empty.title')">{{ t("history.empty.body") }}</EmptyState>
  <section v-else :class="['event-stream', { panel: !embedded, 'event-preview': preview }]" :aria-labelledby="preview ? 'history-preview-title' : undefined">
    <div v-if="preview" class="panel-header">
      <h2 id="history-preview-title">{{ t("history.title") }} <span class="count mono">{{ total.toLocaleString() }}</span></h2>
      <button v-if="total > 0" type="button" class="btn btn-link btn-sm" @click="emit('more')">{{ t("history.full") }}</button>
    </div>
    <div v-if="!preview" class="event-stream-head history-views">
      <div class="event-sources" role="group" :aria-label="t('history.view.label')">
        <button v-for="v in VIEWS" :key="v" type="button" :class="['chip', 'event-source-filter', { selected: view === v }]" :aria-pressed="view === v" @click="view = v">
          {{ viewLabel(v) }}
        </button>
      </div>
      <button v-if="view === 'timeline' && picked.length > 0" type="button" class="btn btn-sm" :disabled="picked.length < 2" @click="comparePicked">
        {{ t("history.pick.compare", { n: picked.length }) }}
      </button>
    </div>
    <HistoryPointInTime
      v-if="!preview && view !== 'timeline'"
      v-model:at="asOf"
      v-model:from="compareFrom"
      v-model:to="compareTo"
      :ci="ci"
      :mode="view"
      @back="view = 'timeline'"
    />
    <div v-else-if="!preview" class="event-stream-head">
      <div class="event-sources" role="group" :aria-label="t('history.sources')">
        <span class="muted">{{ t("history.col.source") }}</span>
        <button
          v-for="s in EVENT_SOURCES"
          :key="s.source"
          type="button"
          :class="['chip', 'event-source-filter', { selected: sources.includes(s.source) }]"
          :aria-pressed="sources.includes(s.source)"
          @click="toggleSource(s.source)"
        >
          {{ sourceLabel(s.source) }}
        </button>
        <button v-if="sources.length > 0" type="button" class="btn btn-link btn-sm" @click="sources = []">{{ t("history.sources.all") }}</button>
      </div>
      <span class="muted">{{ t("history.order") }}</span>
    </div>
    <template v-if="preview || view === 'timeline'">
      <ErrorAlert v-if="log.isError.value" :error="log.error.value" :on-retry="() => log.refetch()" />
      <p v-if="total === 0" class="event-stream-none muted" role="status">{{ preview ? t("history.empty.body") : t("history.sources.none") }}</p>
      <ol v-else class="event-timeline" :aria-busy="log.isFetching.value || undefined">
        <li v-for="e in entries" :key="e.id" class="event" :data-tone="dotTone(e)">
          <span class="event-dot" aria-hidden="true" />
          <div class="event-body">
            <p class="event-head">
              <AuditActor :entry="e" />
              <span :class="['event-action', actionTone(e.action)]" :title="e.action">{{ actionLabel(e.action) }}</span>
              <span class="chip event-source" :title="t('history.col.source')">{{ sourceLabel(eventSource(e)) }}</span>
              <span v-if="!preview" class="event-pick">
                <button type="button" class="btn btn-link btn-sm" :aria-label="t('history.pick.asOfAria', { time: formatUtc(e.occurredAt) })" @click="viewAsOf(e)">
                  {{ t("history.pick.asOf") }}
                </button>
                <label class="check">
                  <input type="checkbox" :checked="isPicked(e)" :aria-label="t('history.pick.selectAria', { time: formatUtc(e.occurredAt) })" @change="togglePick(e)" />
                  <span aria-hidden="true">{{ t("history.pick.select") }}</span>
                </label>
              </span>
            </p>
            <HistoryWorkflowStep v-if="e.action.startsWith('workflow.')" :entry="e" :field-def="fieldDef" />
            <!-- A creation is said by the event itself; there is no diff to show. -->
            <template v-else-if="e.action === 'create'" />
            <p v-else-if="e.action === 'delete'" class="event-change muted">{{ t("history.deleted") }}</p>
            <p v-else-if="e.redacted" class="event-change muted">{{ t("history.redacted") }}</p>
            <p v-else-if="changes(e).length === 0" class="event-change muted">{{ t("history.noChanges") }}</p>
            <ul v-else class="diff event-change">
              <li v-for="c in changes(e)" :key="c.key">
                <code>{{ c.key }}</code>
                <span class="sr-only">{{ t("history.diff.from") }}</span>
                <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del><span v-else class="diff-none" aria-hidden="true">—</span>
                <Icon name="arrow-right" :size="12" class="diff-arrow" />
                <span class="sr-only">{{ t("history.diff.to") }}</span>
                <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">{{ t("history.diff.cleared") }}</span>
              </li>
            </ul>
          </div>
          <time class="event-time mono" :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatUtc(e.occurredAt) }}</time>
        </li>
      </ol>
      <PaginationBar v-if="!preview && total > paging.limit" :total="total" :limit="paging.limit" :offset="paging.offset" @change="(p) => (paging = p)" />
    </template>
  </section>
</template>
