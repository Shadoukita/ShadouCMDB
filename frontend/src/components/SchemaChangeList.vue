<script setup lang="ts">
import type { SchemaChange } from "../api/schemaChanges";

/** Schema changes (ran, or would run) with their exact SQL, each folded to its one-line summary. */
defineProps<{ changes: readonly SchemaChange[]; open?: boolean }>();
</script>

<template>
  <details v-for="c in changes" :key="c.id" class="import-changes" :open="open">
    <summary>{{ c.summary }} <span class="muted">({{ c.statements.length }} {{ c.statements.length === 1 ? "statement" : "statements" }})</span></summary>
    <ul v-if="c.impact.length" class="sc-impact">
      <li v-for="(x, i) in c.impact" :key="i"><span class="badge">{{ x.kind.replace(/_/g, " ") }}</span> {{ x.message }}</li>
    </ul>
    <ol class="sc-ddl">
      <li v-for="(sql, i) in c.statements" :key="i"><pre>{{ sql }}</pre></li>
    </ol>
  </details>
</template>
