<script setup lang="ts">
import { computed, ref } from "vue";
import { useSchemaChanges } from "../../../api/schemaChanges";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { formatDateTime } from "../../../lib/format";

/** The DDL the data model administration ran (cmdb.schema_changes), newest first; each row opens to its exact SQL. */
const limit = ref(25);
const offset = ref(0);
const changes = useSchemaChanges(() => ({ limit: limit.value, offset: offset.value, sort: "-occurredAt" }));
const rows = computed(() => changes.data.value?.data ?? []);
</script>

<template>
  <section class="panel" aria-labelledby="sc-history-title">
    <div class="panel-header">
      <h2 id="sc-history-title">Database changes</h2>
      <span class="muted">Every schema, table and column change, with the SQL that ran</span>
      <span v-if="changes.isFetching.value && !changes.isLoading.value" class="spinner" aria-label="Loading" />
    </div>
    <LoadingState v-if="changes.isLoading.value" label="Loading database changes…" />
    <div v-else-if="changes.isError.value" class="panel-body">
      <ErrorAlert :error="changes.error.value" :on-retry="() => changes.refetch()" />
    </div>
    <div v-else-if="rows.length === 0" class="panel-body"><p class="muted" style="margin: 0">No database changes yet.</p></div>
    <div v-else class="table-wrap">
      <table class="data schema-changes">
        <thead>
          <tr>
            <th scope="col">When</th>
            <th scope="col">Who</th>
            <th scope="col">Change</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="c in rows" :key="c.id">
            <td class="nowrap">{{ formatDateTime(c.occurredAt) }}</td>
            <td>{{ c.actorName ?? c.actorType }}</td>
            <td>
              <details>
                <summary>{{ c.summary }} <span class="muted">({{ c.statements.length }} {{ c.statements.length === 1 ? "statement" : "statements" }})</span></summary>
                <ol class="sc-ddl">
                  <li v-for="(sql, i) in c.statements" :key="i"><pre>{{ sql }}</pre></li>
                </ol>
                <ul v-if="c.impact.length" class="sc-impact">
                  <li v-for="(x, i) in c.impact" :key="i"><span class="badge">{{ x.kind.replace(/_/g, " ") }}</span> {{ x.message }}</li>
                </ul>
              </details>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <PaginationBar
      v-if="changes.data.value && changes.data.value.page.total > 25"
      :total="changes.data.value.page.total"
      :limit="limit"
      :offset="offset"
      @change="(n) => ((limit = n.limit), (offset = n.offset))"
    />
  </section>
</template>
