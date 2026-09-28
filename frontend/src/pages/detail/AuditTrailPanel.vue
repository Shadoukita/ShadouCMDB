<script setup lang="ts">
import { computed } from "vue";
import { useAuditLog, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";

/**
 * The CI's audit trail, placed by a class layout (section kind `audit`): who did
 * what when, with the request id to trace it in the server logs. The History
 * panel shows the same entries as field changes. Needs audit.view (the page
 * leaves the section out otherwise).
 */
const props = defineProps<{ ci: Ci }>();
const log = useAuditLog(() => props.ci.id);
const entries = computed(() => log.data.value?.data ?? []);
</script>

<template>
  <LoadingState v-if="log.isLoading.value" label="Loading the audit trail…" />
  <ErrorAlert v-else-if="log.isError.value" :error="log.error.value" :on-retry="() => log.refetch()" />
  <EmptyState v-else-if="entries.length === 0" title="No audit entries">Nothing has been recorded for this CI yet.</EmptyState>
  <template v-else>
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">When</th>
            <th scope="col">Action</th>
            <th scope="col">By</th>
            <th scope="col">Request id</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="e.id">
            <td>{{ formatDateTime(e.occurredAt) }}</td>
            <td><span class="badge">{{ e.action }}</span></td>
            <td><AuditActor :entry="e" /></td>
            <td class="mono">{{ e.requestId ?? "" }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="log.data.value && log.data.value.page.total > entries.length" class="pagination">
      Showing the latest {{ entries.length }} of {{ log.data.value.page.total }} entries.
    </div>
  </template>
</template>
