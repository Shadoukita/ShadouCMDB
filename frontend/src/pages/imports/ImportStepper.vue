<script setup lang="ts">
import { RouterLink } from "vue-router";
import { STEPS, type WizardStep } from "../../lib/imports";

/**
 * 1 Upload · 2 Map columns · 3 Check · 4 Import (§1.4): an ordered list, the current step marked with
 * aria-current, steps that can be reopened as links, later steps as plain text.
 */
defineProps<{ current: WizardStep; last: WizardStep; linkTo: (step: WizardStep) => string | undefined }>();
</script>

<template>
  <nav class="import-stepper" aria-label="Import steps">
    <ol>
      <li
        v-for="s in STEPS"
        :key="s.step"
        :class="{ current: s.step === current, done: s.step < current, future: s.step > last }"
        :aria-current="s.step === current ? 'step' : undefined"
      >
        <span class="num" aria-hidden="true">{{ s.step < current ? "✓" : s.step }}</span>
        <RouterLink v-if="s.step !== current && s.step <= last && linkTo(s.step)" :to="linkTo(s.step)!">
          {{ s.label }}<span v-if="s.step < current" class="sr-only"> (done)</span>
        </RouterLink>
        <span v-else>{{ s.label }}<span v-if="s.step < current" class="sr-only"> (done)</span></span>
      </li>
    </ol>
  </nav>
</template>
