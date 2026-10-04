<script setup lang="ts">
import type { WorkflowWarning } from "../../../api/workflows";
import { uninstancedText } from "./uninstanced";

/**
 * The API's warning after activating a workflow that drives a state field (or giving an active one a state field).
 * `bootstrapTarget`: the id of the bootstrap panel on the page, which the warning links to.
 */
const props = defineProps<{ warning: WorkflowWarning; bootstrapTarget?: string }>();

function goToBootstrap() {
  const el = props.bootstrapTarget ? document.getElementById(props.bootstrapTarget) : null;
  el?.scrollIntoView({ block: "start" });
  el?.focus({ preventScroll: true });
}
</script>

<template>
  <div v-if="warning.code === 'UNINSTANCED_CIS'" :class="['alert', { 'alert-warn': warning.count !== 0 }]" role="status" data-testid="wf-uninstanced">
    <strong>{{ warning.count === null ? "CIs without an instance" : `${warning.count.toLocaleString()} CIs without an instance` }}</strong>
    <div>{{ uninstancedText(warning) }}</div>
    <div v-if="bootstrapTarget && warning.count !== 0">
      <button type="button" class="btn btn-sm" data-testid="wf-uninstanced-bootstrap" @click="goToBootstrap">Adopt them with the bootstrap</button>
    </div>
  </div>
  <div v-else class="alert alert-warn" role="status">{{ warning.message }}</div>
</template>
