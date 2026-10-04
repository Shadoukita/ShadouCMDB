<script setup lang="ts">
import { t } from "../i18n";
import Icon from "./Icon.vue";

/**
 * The save bar of a record (design document §2.7, audit R11): docked to the bottom of the page while
 * the fields scroll, the state on the left, the actions on the right with the primary last.
 * On a CI or service page it appears once something was changed and is the region "Unsaved changes";
 * on the edit and create pages it is always there (`status` empty while nothing was changed).
 * The actions come from the default slot.
 */
defineProps<{
  /** The region's accessible name. */
  label: string;
  /** Something was changed: "Unsaved changes" and, when known, how many fields. */
  dirty?: boolean;
  /** Changed fields, when the page counts them. */
  changes?: number;
}>();
</script>

<template>
  <div class="save-bar record-save-bar" role="region" :aria-label="label">
    <p v-if="dirty" class="save-bar-status">
      <Icon name="circle-alert" :size="16" />
      <strong>{{ t("record.save.unsaved") }}</strong>
      <span v-if="changes" class="save-bar-count">{{ t("record.save.changes", { n: changes }) }}</span>
    </p>
    <div class="save-bar-actions">
      <slot />
    </div>
  </div>
</template>
