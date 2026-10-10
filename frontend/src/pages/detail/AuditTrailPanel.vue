<script setup lang="ts">
import { computed } from "vue";
import { useAuditLog, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { actionLabel, actionTone, formatUtc } from "../../lib/auditEvents";
import { formatDateTime } from "../../lib/format";
import AuditActor from "../admin/AuditActor.vue";

/**
 * The CI's audit trail, placed by a class layout (section kind `audit`): who did
 * what when, with the request id to trace it in the server logs. The History
 * panel shows the same entries as field changes. Needs audit.view (the page
 * leaves the section out otherwise). Times and action badges read as in the History
 * event stream (audit R8).
 */
const props = defineProps<{ ci: Ci }>();
const log = useAuditLog(() => props.ci.id);
const entries = computed(() => log.data.value?.data ?? []);
</script>

<template>
  <LoadingState v-if="log.isLoading.value" :label="t('record.audit.loading')" />
  <ErrorAlert v-else-if="log.isError.value" :error="log.error.value" :on-retry="() => log.refetch()" />
  <EmptyState v-else-if="entries.length === 0" :title="t('record.audit.empty')">{{ t("record.audit.emptyBody") }}</EmptyState>
  <template v-else>
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">{{ t("history.col.time") }}</th>
            <th scope="col">{{ t("audit.col.action") }}</th>
            <th scope="col">{{ t("record.audit.col.by") }}</th>
            <th scope="col">{{ t("error.requestId") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="e.id">
            <td class="mono"><time :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatUtc(e.occurredAt) }}</time></td>
            <td><span :class="['badge', actionTone(e.action)]" :title="e.action">{{ actionLabel(e.action) }}</span></td>
            <td><AuditActor :entry="e" /></td>
            <td class="mono">{{ e.requestId ?? "" }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="log.data.value && log.data.value.page.total > entries.length" class="pagination">
      {{ t("record.audit.showing", { n: entries.length, total: log.data.value.page.total }) }}
    </div>
  </template>
</template>
