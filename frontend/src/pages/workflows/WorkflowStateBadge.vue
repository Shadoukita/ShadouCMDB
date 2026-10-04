<script setup lang="ts">
import { CATEGORY_TONES, type WorkflowStateRef } from "../../api/workflowRuntime";
import { CATEGORIES } from "../../lib/workflowDraft";

/** A workflow state as a badge, toned by its category (open, in progress, done, cancelled). */
defineProps<{ state: Pick<WorkflowStateRef, "name" | "category"> & { terminal?: boolean } }>();
const categoryLabel = (c: string) => CATEGORIES.find((x) => x.value === c)?.label ?? c;
</script>

<template>
  <span :class="['badge', CATEGORY_TONES[state.category]]" :title="`${categoryLabel(state.category)}${state.terminal ? ', final state' : ''}`" dir="auto">{{
    state.name
  }}</span>
</template>
