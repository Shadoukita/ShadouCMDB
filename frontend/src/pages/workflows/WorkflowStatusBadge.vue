<script setup lang="ts">
import Icon from "../../components/Icon.vue";
import { STATUS_ICONS, STATUS_LABELS, STATUS_TONES, type WorkflowInstanceStatus } from "../../api/workflowRuntime";
import { t } from "../../i18n";

/** An instance's status (running, completed, cancelled) as a pill with icon and text, never colour alone. */
defineProps<{ status: WorkflowInstanceStatus }>();
</script>

<template>
  <span :class="['badge', STATUS_TONES[status]]"><Icon :name="STATUS_ICONS[status]" :size="14" /> {{ t(STATUS_LABELS[status]) }}</span>
</template>
