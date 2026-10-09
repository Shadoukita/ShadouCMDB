<script setup lang="ts">
import { RouterLink } from "vue-router";
import { STEPS, type WizardStep } from "../../lib/imports";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";

/**
 * 1 Upload · 2 Map columns · 3 Check · 4 Import (§1.4, audit M1): an ordered list with connectors, the current
 * step marked with aria-current, finished steps with a check, steps that can be reopened as links, later steps as
 * plain text. Done and upcoming are told apart by the icon and the "(done)" text too, never by colour alone.
 */
defineProps<{ current: WizardStep; last: WizardStep; linkTo: (step: WizardStep) => string | undefined }>();
</script>

<template>
  <nav class="import-stepper" :aria-label="t('imports.step.nav')">
    <ol>
      <li
        v-for="s in STEPS"
        :key="s.step"
        :class="{ current: s.step === current, done: s.step < current, future: s.step > last }"
        :aria-current="s.step === current ? 'step' : undefined"
      >
        <span class="num" aria-hidden="true"><Icon v-if="s.step < current" name="check" :size="14" /><template v-else>{{ s.step }}</template></span>
        <RouterLink v-if="s.step !== current && s.step <= last && linkTo(s.step)" :to="linkTo(s.step)!" class="label">
          {{ t(s.label) }}<span v-if="s.step < current" class="sr-only">{{ t("imports.step.done") }}</span>
        </RouterLink>
        <span v-else class="label">{{ t(s.label) }}<span v-if="s.step < current" class="sr-only">{{ t("imports.step.done") }}</span></span>
      </li>
    </ol>
  </nav>
</template>
