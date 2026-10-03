<script setup lang="ts">
import type { WorkflowWarning } from "../../../api/workflows";
import { uninstancedText } from "./uninstanced";

/** The API's warning after activating a workflow that drives a state field (or giving an active one a state field). */
defineProps<{ warning: WorkflowWarning }>();
</script>

<template>
  <div v-if="warning.code === 'UNINSTANCED_CIS'" :class="['alert', { 'alert-warn': warning.count !== 0 }]" role="status" data-testid="wf-uninstanced">
    <strong>{{ warning.count === null ? "CIs without an instance" : `${warning.count.toLocaleString()} CIs without an instance` }}</strong>
    <div>{{ uninstancedText(warning) }}</div>
  </div>
  <div v-else class="alert alert-warn" role="status">{{ warning.message }}</div>
</template>
