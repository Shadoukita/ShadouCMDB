<script setup lang="ts">
import { computed } from "vue";
import { useAuditLog, type AuditEntry, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";

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

function changes(entry: AuditEntry) {
  const oldV = flatten(entry.oldValue);
  const newV = flatten(entry.newValue);
  const keys = [...new Set([...Object.keys(oldV), ...Object.keys(newV)])].filter(
    (k) => !HIDDEN.has(k.split(".")[0]) && oldV[k] !== newV[k],
  );
  return keys.map((k) => ({ key: k, old: oldV[k], new: newV[k] }));
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
            <td style="padding-top: 5px"><span :class="['badge', actionTone(e.action)]">{{ e.action }}</span></td>
            <td style="padding-top: 5px"><AuditActor :entry="e" /></td>
            <td style="white-space: normal; padding-top: 5px; padding-bottom: 5px">
              <span v-if="e.action === 'create'" class="muted">Created</span>
              <span v-else-if="e.action === 'delete'" class="muted">Deleted (relationships removed with it)</span>
              <span v-else-if="changes(e).length === 0" class="muted">No visible field changes</span>
              <ul v-else class="diff">
                <li v-for="c in changes(e)" :key="c.key">
                  <code>{{ c.key }}</code>: <del v-if="c.old !== undefined">{{ c.old }}</del> →
                  <ins v-if="c.new !== undefined">{{ c.new }}</ins><span v-else class="muted">cleared</span>
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
