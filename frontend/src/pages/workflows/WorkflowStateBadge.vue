<script setup lang="ts">
import { CATEGORY_TONES, type WorkflowStateRef } from "../../api/workflowRuntime";
import { t } from "../../i18n";
import { categoryLabel } from "../../lib/workflowDraft";

/** A workflow state as a badge, toned by its category (open, in progress, done, cancelled). */
defineProps<{ state: Pick<WorkflowStateRef, "name" | "category"> & { terminal?: boolean } }>();
</script>

<template>
  <span
    :class="['badge', CATEGORY_TONES[state.category]]"
    :title="state.terminal ? t('wfRun.stateTitleFinal', { category: categoryLabel(state.category) }) : categoryLabel(state.category)"
    dir="auto"
    >{{ state.name }}</span
  >
</template>
