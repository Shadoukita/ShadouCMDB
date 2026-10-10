<script setup lang="ts">
import type { SchemaChange } from "../api/schemaChanges";
import { t } from "../i18n";
import { DESTRUCTIVE_IMPACT, impactLabel } from "../lib/schemaChange";

/** Schema changes (ran, or would run) with their exact SQL, each folded to its one-line summary. */
defineProps<{ changes: readonly SchemaChange[]; open?: boolean }>();
</script>

<template>
  <details v-for="c in changes" :key="c.id" class="import-changes" :open="open">
    <summary>{{ c.summary }} <span class="muted">({{ t("dm.schemaChanges.statements", { n: c.statements.length }) }})</span></summary>
    <ul v-if="c.impact.length" class="sc-impact">
      <li v-for="(x, i) in c.impact" :key="i" :class="{ destructive: DESTRUCTIVE_IMPACT.has(x.kind) }">
        <span :class="['badge', { danger: DESTRUCTIVE_IMPACT.has(x.kind) }]">{{ impactLabel(x.kind) }}</span> {{ x.message }}
      </li>
    </ul>
    <ol class="sc-ddl">
      <li v-for="(sql, i) in c.statements" :key="i"><pre>{{ sql }}</pre></li>
    </ol>
  </details>
</template>
