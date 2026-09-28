<script setup lang="ts">
import type { Ci } from "../../api/queries";
import NoteText from "../../components/NoteText.vue";
import type { TrailStep } from "../../lib/trail";
import type { SectionKind } from "../../lib/uiSettings";
import AuditTrailPanel from "./AuditTrailPanel.vue";
import HistoryPanel from "./HistoryPanel.vue";
import RelationshipsPanel from "./RelationshipsPanel.vue";

/** The body of a layout content block on the detail page: a note's text, or a built-in panel under the section's heading. */
defineProps<{ kind: SectionKind; text?: string; ci: Ci; self: TrailStep; trail: TrailStep[] }>();
</script>

<template>
  <div v-if="kind === 'note'" class="panel-body"><NoteText :text="text ?? ''" /></div>
  <div v-else class="panel-body flush">
    <RelationshipsPanel v-if="kind === 'relations'" :ci="ci" :self="self" :trail="trail" embedded />
    <HistoryPanel v-else-if="kind === 'history'" :ci="ci" embedded />
    <AuditTrailPanel v-else-if="kind === 'audit'" :ci="ci" />
  </div>
</template>
