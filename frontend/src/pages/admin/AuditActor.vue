<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import type { AuditEntry } from "../../api/queries";
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
const TYPE_LABEL: Record<string, string> = { system: "system", user: "user", api_client: "API client", import: "import" };
</script>

<template>
  <span class="actor">
    <RouterLink v-if="to && entry.actorName" :to="to" dir="auto">{{ entry.actorName }}</RouterLink>
    <bdi v-else-if="entry.actorName">{{ entry.actorName }}</bdi>
    <span v-else class="muted">unknown</span>
    <span v-if="entry.actorType !== 'user'" class="muted"> ({{ TYPE_LABEL[entry.actorType] ?? entry.actorType }})</span>
  </span>
</template>
