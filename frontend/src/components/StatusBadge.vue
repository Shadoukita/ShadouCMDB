<script setup lang="ts">
const TONES: Record<string, string> = {
  in_service: "ok",
  active: "ok",
  operational: "ok",
  planned: "neutral",
  ordered: "neutral",
  in_stock: "neutral",
  maintenance: "warn",
  degraded: "warn",
  retired: "off",
  decommissioned: "off",
  disposed: "off",
  failed: "danger",
};

/** Status badge. Tone is a hint derived from the status key; unknown keys stay neutral. */
defineProps<{ status: { key: string; name: string } | null | undefined }>();
</script>

<template>
  <span v-if="status" :class="['badge', TONES[status.key] ?? '']">{{ status.name }}</span>
</template>
