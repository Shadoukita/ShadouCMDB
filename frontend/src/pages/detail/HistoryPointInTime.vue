<script setup lang="ts">
import { computed } from "vue";
import { useAuditPurges, useCiHistoryWalk, useClassAttributes, type Ci, type EffectiveAttribute } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { actionLabel, formatUtc } from "../../lib/auditEvents";
import {
  changesBetween,
  diffSnapshots,
  fieldKeys,
  fieldValue,
  HISTORY_WALK_LIMIT,
  isoToUtcInput,
  purgeCutoff,
  stateAt,
  utcInputToIso,
  walkStart,
  workflowEventsBetween,
  type FieldKey,
  type PointState,
  type Snapshot,
} from "../../lib/ciHistory";
import { formatDateTime } from "../../lib/format";
import { fieldLabel } from "../../lib/uiSettings";
import AuditActor from "../admin/AuditActor.vue";
import HistoryFieldValue from "./HistoryFieldValue.vue";
import HistoryWorkflowStep from "./HistoryWorkflowStep.vue";

/**
 * The CI at a past moment ("as of") or the net difference between two moments ("compare"), rebuilt in the
 * browser from its audit history (SHAA-3131, lib/ciHistory): read-only, every time in UTC. The moments are
 * ISO 8601 strings owned by the History tab, so a timeline entry can set them.
 */
const props = defineProps<{ ci: Ci; mode: "asOf" | "compare" }>();
const at = defineModel<string>("at", { required: true });
const from = defineModel<string>("from", { required: true });
const to = defineModel<string>("to", { required: true });
const emit = defineEmits<{ back: [] }>();

const purges = useAuditPurges();
const cutoff = computed(() => purgeCutoff(purges.data.value ?? []));
const earliest = computed(() => (props.mode === "asOf" ? at.value : from.value < to.value ? from.value : to.value));
const since = computed(() => (purges.data.value ? walkStart(earliest.value, props.ci.createdAt, cutoff.value) : null));
const walk = useCiHistoryWalk(() => props.ci.id, since);
const loading = computed(() => purges.isLoading.value || walk.isLoading.value || (!!since.value && walk.data.value?.since !== since.value && walk.isFetching.value));
const failed = computed(() => (purges.isError.value ? purges : walk.isError.value ? walk : null));

/** The state at a moment, once a walk that covers it has loaded (not the previous one kept on screen). */
function state(moment: string): PointState | null {
  const w = walk.data.value;
  return w && w.since <= walkStart(moment, props.ci.createdAt, cutoff.value) ? stateAt(props.ci, w, moment, cutoff.value) : null;
}
/** Compare reads the earlier moment as "from" whichever picker holds it. */
const range = computed(() => (from.value <= to.value ? [from.value, to.value] : [to.value, from.value]) as [string, string]);
const asOf = computed(() => (props.mode === "asOf" ? state(at.value) : null));
const fromState = computed(() => (props.mode === "compare" ? state(range.value[0]) : null));
const toState = computed(() => (props.mode === "compare" ? state(range.value[1]) : null));

/** Field definitions of the CI's class and of the class it had at that moment (retired ones too), so values show by name. */
const shownClass = computed(() => {
  const s = props.mode === "asOf" ? asOf.value : fromState.value;
  return s?.kind === "snapshot" && s.snapshot.classId && s.snapshot.classId !== props.ci.classId ? s.snapshot.classId : undefined;
});
const attrs = useClassAttributes(() => props.ci.classId, { includeInactive: true });
const pastAttrs = useClassAttributes(shownClass, { includeInactive: true });
const defs = computed<EffectiveAttribute[]>(() => [...(shownClass.value ? (pastAttrs.data.value ?? []) : []), ...(attrs.data.value ?? [])]);
const defFor = (key: FieldKey) => (key.startsWith("attributes.") ? defs.value.find((d) => d.key === key.slice("attributes.".length)) : undefined);
const order = computed(() => defs.value.map((d) => d.key));
const label = (key: FieldKey) => fieldLabel(key, defs.value);
const self = computed(() => ({ id: props.ci.id, name: props.ci.label }));

/** As of: the built-in fields, the class's current fields (with or without a value), and any other recorded attribute. */
const asOfFields = computed(() => {
  const s = asOf.value;
  if (s?.kind !== "snapshot") return [];
  const show = (s.snapshot.classId === props.ci.classId ? (attrs.data.value ?? []) : (pastAttrs.data.value ?? [])).filter((d) => d.isActive !== false).map((d) => d.key);
  return fieldKeys([s.snapshot], order.value, show).map((key) => ({ key, value: fieldValue(s.snapshot, key) }));
});
const hasGaps = (s: Snapshot, keys: readonly FieldKey[]) => keys.some((k) => ["hidden", "notRecorded"].includes(fieldValue(s, k).kind));

/** A moment before the CI existed compares as a CI with no values. */
const EMPTY: Snapshot = { fields: {}, rest: { kind: "unset" }, refs: {}, classId: null, label: null, deleted: null };
const comparable = (s: PointState | null) => (s?.kind === "snapshot" ? s.snapshot : s?.kind === "notYet" ? EMPTY : null);
const changes = computed(() => {
  const a = comparable(fromState.value);
  const b = comparable(toState.value);
  return a && b ? diffSnapshots(a, b, order.value) : null;
});
const events = computed(() => (walk.data.value && props.mode === "compare" ? workflowEventsBetween(walk.data.value, ...range.value) : []));
const changeCount = computed(() => (walk.data.value && props.mode === "compare" ? changesBetween(walk.data.value, ...range.value) : 0));
const refOf = (s: PointState | null, key: FieldKey) => (s?.kind === "snapshot" && key.startsWith("attributes.") ? s.snapshot.refs[key.slice("attributes.".length)] : undefined);

/** The pickers edit UTC minutes; an incomplete value keeps the last valid moment. */
const nowInput = () => isoToUtcInput(new Date().toISOString());
const models = { at, from, to };
function setMoment(which: keyof typeof models, e: Event) {
  const iso = utcInputToIso((e.target as HTMLInputElement).value);
  if (iso) models[which].value = iso;
}
const fieldId = (s: string) => `history-pit-${s}-${props.ci.id}`;
</script>

<template>
  <div class="history-pit" :data-mode="mode">
    <div class="history-pit-controls" role="group" :aria-label="mode === 'asOf' ? t('history.pit.asOfControls') : t('history.pit.compareControls')">
      <template v-if="mode === 'asOf'">
        <label :for="fieldId('at')">{{ t("history.pit.asOfLabel") }}</label>
        <input :id="fieldId('at')" class="input input-sm mono" type="datetime-local" step="60" :max="nowInput()" :value="isoToUtcInput(at)" @change="setMoment('at', $event)" />
      </template>
      <template v-else>
        <label :for="fieldId('from')">{{ t("history.pit.fromLabel") }}</label>
        <input :id="fieldId('from')" class="input input-sm mono" type="datetime-local" step="60" :max="nowInput()" :value="isoToUtcInput(from)" @change="setMoment('from', $event)" />
        <label :for="fieldId('to')">{{ t("history.pit.toLabel") }}</label>
        <input :id="fieldId('to')" class="input input-sm mono" type="datetime-local" step="60" :max="nowInput()" :value="isoToUtcInput(to)" @change="setMoment('to', $event)" />
      </template>
      <button type="button" class="btn btn-sm" @click="emit('back')"><Icon name="arrow-left" :size="14" /> {{ t("history.pit.back") }}</button>
    </div>

    <LoadingState v-if="loading" :label="t('history.pit.loading')" />
    <ErrorAlert v-else-if="failed" :error="failed.error.value" :on-retry="() => failed?.refetch()" />

    <!-- As of: the CI's fields at that moment, read-only. -->
    <section v-else-if="mode === 'asOf' && asOf" class="history-pit-body" :aria-labelledby="fieldId('as-of-title')" aria-live="polite">
      <h3 :id="fieldId('as-of-title')" class="history-pit-banner">
        <Icon name="scroll-text" :size="14" />
        <span>{{ t("history.pit.asOfTitle", { time: formatUtc(at) }) }}</span>
        <span class="chip">{{ t("history.pit.readOnly") }}</span>
      </h3>
      <p v-if="asOf.kind === 'notYet'" class="history-pit-note muted" role="status">{{ t("history.pit.notYet", { time: formatUtc(asOf.createdAt) }) }}</p>
      <p v-else-if="asOf.kind === 'purged'" class="history-pit-note muted" role="status">{{ t("history.pit.purged", { time: formatUtc(asOf.before) }) }}</p>
      <p v-else-if="asOf.kind === 'tooMany'" class="history-pit-note muted" role="status">
        {{ t("history.pit.tooMany", { n: asOf.total, limit: HISTORY_WALK_LIMIT.toLocaleString() }) }}
      </p>
      <template v-else>
        <p v-if="asOf.snapshot.deleted" class="history-pit-note" role="status">{{ t("history.pit.wasDeleted") }}</p>
        <p v-if="hasGaps(asOf.snapshot, asOfFields.map((f) => f.key))" class="history-pit-note muted">{{ t("history.pit.gaps") }}</p>
        <dl class="props history-pit-fields">
          <template v-for="f in asOfFields" :key="f.key">
            <dt :title="f.key">{{ label(f.key) }}</dt>
            <dd :data-field="f.key"><HistoryFieldValue :field="f.key" :value="f.value" :def="defFor(f.key)" :ref-info="refOf(asOf, f.key)" :self="self" /></dd>
          </template>
        </dl>
      </template>
    </section>

    <!-- Compare: the net difference per field, and the workflow steps in between. -->
    <section v-else-if="mode === 'compare'" class="history-pit-body" :aria-labelledby="fieldId('compare-title')" aria-live="polite">
      <h3 :id="fieldId('compare-title')" class="history-pit-banner">
        <Icon name="scroll-text" :size="14" />
        <span>{{ t("history.pit.compareTitle", { from: formatUtc(range[0]), to: formatUtc(range[1]) }) }}</span>
        <span class="chip">{{ t("history.pit.readOnly") }}</span>
      </h3>
      <template v-for="[side, s] in [[range[0], fromState], [range[1], toState]] as const" :key="side">
        <p v-if="s?.kind === 'notYet'" class="history-pit-note muted" role="status">{{ t("history.pit.notYetAt", { at: formatUtc(side), time: formatUtc(s.createdAt) }) }}</p>
        <p v-else-if="s?.kind === 'purged'" class="history-pit-note muted" role="status">{{ t("history.pit.purged", { time: formatUtc(s.before) }) }}</p>
        <p v-else-if="s?.kind === 'snapshot' && s.snapshot.deleted" class="history-pit-note" role="status">{{ t("history.pit.wasDeletedAt", { at: formatUtc(side) }) }}</p>
      </template>
      <p v-if="fromState?.kind === 'tooMany'" class="history-pit-note muted" role="status">
        {{ t("history.pit.tooMany", { n: fromState.total, limit: HISTORY_WALK_LIMIT.toLocaleString() }) }}
      </p>
      <template v-else-if="changes">
        <p class="history-pit-note muted">{{ t("history.pit.changeCount", { n: changeCount }) }}</p>
        <p v-if="changes.length === 0" class="history-pit-note muted" role="status">{{ t("history.pit.noDifference") }}</p>
        <ul v-else class="diff history-pit-diff" :aria-label="t('history.pit.diffLabel')">
          <li v-for="c in changes" :key="c.key" :data-field="c.key">
            <span class="history-pit-field" :title="c.key">{{ label(c.key) }}</span>
            <span class="sr-only">{{ t("history.diff.from") }}</span>
            <del v-if="c.from.kind === 'value'" dir="auto"><HistoryFieldValue :field="c.key" :value="c.from" :def="defFor(c.key)" :ref-info="refOf(fromState, c.key)" :self="self" /></del>
            <span v-else :class="c.from.kind === 'unset' ? 'diff-none' : 'muted'"><HistoryFieldValue :field="c.key" :value="c.from" :self="self" /></span>
            <Icon name="arrow-right" :size="12" class="diff-arrow" />
            <span class="sr-only">{{ t("history.diff.to") }}</span>
            <ins v-if="c.to.kind === 'value'" dir="auto"><HistoryFieldValue :field="c.key" :value="c.to" :def="defFor(c.key)" :ref-info="refOf(toState, c.key)" :self="self" /></ins>
            <span v-else-if="c.to.kind === 'unset'" class="muted">{{ t("history.diff.cleared") }}</span>
            <span v-else class="muted"><HistoryFieldValue :field="c.key" :value="c.to" :self="self" /></span>
          </li>
        </ul>
        <template v-if="events.length > 0">
          <h4 class="history-pit-subhead">{{ t("history.pit.workflowEvents", { n: events.length }) }}</h4>
          <ol class="event-timeline history-pit-events">
            <li v-for="e in events" :key="e.id" class="event" data-tone="info">
              <span class="event-dot" aria-hidden="true" />
              <div class="event-body">
                <p class="event-head">
                  <AuditActor :entry="e" />
                  <span class="event-action info" :title="e.action">{{ actionLabel(e.action) }}</span>
                </p>
                <HistoryWorkflowStep :entry="e" :field-def="(k) => defs.find((d) => d.key === k)" />
              </div>
              <time class="event-time mono" :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatUtc(e.occurredAt) }}</time>
            </li>
          </ol>
        </template>
      </template>
    </section>
    <p class="history-pit-foot muted">{{ t("history.pit.relationshipsNote") }}</p>
  </div>
</template>
