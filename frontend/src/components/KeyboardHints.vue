<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";

/**
 * The keyboard hints under an explorer table (design document §2.7, Explorer › Footer bar): the
 * keys in mono for sighted keyboard users, and the same as one sentence (`id`) that the table
 * names in aria-describedby, so a screen reader announces it once when it enters the table.
 * Only the shortcuts the list offers are shown (lib/rowKeyboard).
 */
const props = defineProps<{ id: string; edit?: boolean; columns?: boolean }>();

const keys = computed(() => [
  { key: "↑↓", what: t("explorer.keys.move") },
  { key: t("explorer.keys.enter"), what: t("explorer.keys.open") },
  { key: "/", what: t("explorer.keys.search") },
  ...(props.edit ? [{ key: "e", what: t("explorer.keys.edit") }] : []),
  ...(props.columns ? [{ key: "c", what: t("explorer.keys.columns") }] : []),
]);
const sentence = computed(() =>
  [t("explorer.keys.sr"), props.edit ? t("explorer.keys.sr.edit") : "", props.columns ? t("explorer.keys.sr.columns") : ""].filter(Boolean).join(" "),
);
</script>

<template>
  <div class="kbd-hints">
    <span aria-hidden="true">
      <template v-for="(k, i) in keys" :key="k.key"><template v-if="i > 0"> · </template><kbd>{{ k.key }}</kbd> {{ k.what }}</template>
    </span>
    <span :id="id" class="sr-only">{{ sentence }}</span>
  </div>
</template>
