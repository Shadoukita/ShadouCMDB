<script setup lang="ts">
import type { ActionDelivery } from "../../../api/actionDeliveries";
import { t } from "../../../i18n";

/** Who or what a delivery goes to: a user by name, a fixed address (masked by the API), or an endpoint. */
defineProps<{ recipient: ActionDelivery["recipient"] }>();
</script>

<template>
  <span v-if="recipient.kind === 'address'" class="mono">{{ recipient.address }}</span>
  <span v-else-if="recipient.name" dir="auto">
    {{ recipient.name }}<span v-if="recipient.username" class="muted mono"> {{ recipient.username }}</span>
  </span>
  <span v-else class="muted">{{ recipient.kind === "endpoint" ? t("deliveries.endpointDeleted") : t("deliveries.userDeleted") }}</span>
</template>
