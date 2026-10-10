<script setup lang="ts">
import { t } from "../../../i18n";
import type { PlacedProblem } from "../../../lib/workflowDraft";

/** The lint's problems about one state or transition, errors first. */
defineProps<{ problems: PlacedProblem[] }>();
</script>

<template>
  <ul v-if="problems.length" class="wf-problems" :aria-label="t('wfDesign.problems')">
    <li v-for="(p, i) in problems" :key="i" :class="p.severity">
      <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
      {{ p.message }}
    </li>
  </ul>
</template>
