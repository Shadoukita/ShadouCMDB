<script setup lang="ts">
import { t } from "../i18n";

defineProps<{ label?: string }>();
</script>

<template>
  <div class="state" role="status" aria-live="polite"><span class="spinner" aria-hidden="true" /> {{ label ?? t("common.loading") }}</div>
</template>
