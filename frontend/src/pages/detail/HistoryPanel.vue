<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAuditLog, useClassAttributes, type AuditEntry, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";
import ChangeValue from "../imports/ChangeValue.vue";

/** Change history from GET /audit-log?entityId=… with a field-level diff of each update. */
/** `embedded`: placed in a layout section, which gives the frame and the heading. */
const props = defineProps<{ ci: Ci; embedded?: boolean }>();

/**
 * Bookkeeping and derived fields (the label follows the title attribute, `active` the validity period).
 * Entries from before CIs kept status, owner etc. in attributes still carry those ids and references.
 */
const HIDDEN = new Set(["updatedAt", "createdAt", "version", "classId", "label", "active", "attributeReferences", "statusId", "environmentId", "ownerId", "locationId"]);
/** Embedded references are shown by name instead of by id. */
const REFS = new Set(["class", "status", "environment", "owner", "location"]);

const log = useAuditLog(() => props.ci.id);
const entries = computed(() => log.data.value?.data ?? []);
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
const WORKFLOW_ACTIONS: Record<string, string> = {
  "workflow.start": "workflow started",
  "workflow.transition": "workflow step",
  "workflow.cancel": "workflow cancelled",
  "workflow.migrate": "workflow migrated",
  "workflow.force": "workflow state forced",
};
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
    note: n.reason === "ci_deleted" ? "the CI was deleted" : (n.comment ?? n.reason ?? undefined),
    fields: Object.entries(n.fields ?? {}).map(([k, v]) => ({ key: k, def: fieldDef(k), old: show(v?.old), new: show(v?.new) })),
  };
}

const actionTone = (action: string) => (action === "delete" ? "danger" : action === "create" ? "ok" : "");
</script>

<template>
  <LoadingState v-if="log.isLoading.value" label="Loading history…" />
  <ErrorAlert v-else-if="log.isError.value" :error="log.error.value" :on-retry="() => log.refetch()" />
  <EmptyState v-else-if="entries.length === 0" title="No recorded changes">This CI has no audit entries yet.</EmptyState>
  <section v-else :class="{ panel: !embedded }">
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">When</th>
            <th scope="col">Action</th>
            <th scope="col">Changed by</th>
            <th scope="col" style="width: 100%">Changes</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="e.id" style="vertical-align: top">
            <td style="padding-top: 5px">{{ formatDateTime(e.occurredAt) }}</td>
            <td style="padding-top: 5px">
              <span :class="['badge', WORKFLOW_ACTIONS[e.action] ? 'info' : actionTone(e.action)]" :title="e.action">{{ WORKFLOW_ACTIONS[e.action] ?? e.action }}</span>
            </td>
            <td style="padding-top: 5px"><AuditActor :entry="e" /></td>
            <td style="white-space: normal; padding-top: 5px; padding-bottom: 5px">
              <template v-if="WORKFLOW_ACTIONS[e.action]">
                <div v-for="w in [workflowStep(e)]" :key="e.id" data-testid="history-workflow">
                  <RouterLink v-if="w.instanceId" :to="`/workflows/${w.instanceId}`" class="mono">{{ w.definitionKey }}</RouterLink>
                  <span v-else class="mono">{{ w.definitionKey }}</span>
                  <template v-if="w.transitionKey">: <code>{{ w.transitionKey }}</code></template>
                  <span v-if="w.to">, <template v-if="w.from"><code>{{ w.from }}</code> → </template><code>{{ w.to }}</code></span>
                  <div v-if="w.note" class="wf-comment muted" dir="auto">{{ w.note }}</div>
                  <ul v-if="w.fields.length > 0" class="diff">
                    <li v-for="c in w.fields" :key="c.key">
                      <code>{{ c.key }}</code>: <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del> →
                      <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">cleared</span>
                    </li>
                  </ul>
                </div>
              </template>
              <span v-else-if="e.action === 'create'" class="muted">Created</span>
              <span v-else-if="e.action === 'delete'" class="muted">Deleted (relationships removed with it)</span>
              <span v-else-if="changes(e).length === 0" class="muted">No visible field changes</span>
              <ul v-else class="diff">
                <li v-for="c in changes(e)" :key="c.key">
                  <code>{{ c.key }}</code>: <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del> →
                  <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">cleared</span>
                </li>
              </ul>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="log.data.value && log.data.value.page.total > entries.length" class="pagination">
      Showing the latest {{ entries.length }} of {{ log.data.value.page.total }} entries.
    </div>
  </section>
</template>
