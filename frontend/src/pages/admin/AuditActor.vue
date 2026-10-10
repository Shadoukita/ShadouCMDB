<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import type { AuditEntry } from "../../api/queries";
import { hasMessage, t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/**
 * Who made a change. A user actor links to that user (with users.manage) or to
 * their other changes (with audit.view); system actors (setup, CLI, seed) say so.
 */
const props = defineProps<{ entry: AuditEntry }>();
const session = useSessionStore();
const isUser = computed(() => props.entry.actorType === "user" && !!props.entry.actorId);
const to = computed(() => {
  if (!isUser.value) return undefined;
  if (session.can("users.manage")) return `/admin/users/${props.entry.actorId}`;
  if (session.can("audit.view")) return { path: "/admin/audit", query: { actorId: props.entry.actorId! } };
  return undefined;
});
const typeLabel = (type: string) => {
  const key = `customization.history.actor.${type}`;
  return hasMessage(key) ? t(key) : type;
};
</script>

<template>
  <span class="actor">
    <RouterLink v-if="to && entry.actorName" :to="to" dir="auto">{{ entry.actorName }}</RouterLink>
    <bdi v-else-if="entry.actorName">{{ entry.actorName }}</bdi>
    <span v-else class="muted">{{ t("audit.actor.unknown") }}</span>
    <span v-if="entry.actorType !== 'user'" class="muted"> ({{ typeLabel(entry.actorType) }})</span>
  </span>
</template>
