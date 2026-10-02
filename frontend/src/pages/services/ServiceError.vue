<script setup lang="ts">
import { computed } from "vue";
import { ApiError } from "../../api/client";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";

/**
 * A failed business service request (spec §5.8): a busy server (429, or 503 SERVER_BUSY from the
 * traversal limits) says so with Retry; anything else is the error panel with the API's message and
 * request id, never a blank page.
 */
const props = defineProps<{ error: unknown; onRetry?: () => void; title?: string }>();
const busy = computed(() => props.error instanceof ApiError && (props.error.status === 429 || props.error.code === "SERVER_BUSY"));
</script>

<template>
  <div v-if="busy" class="alert alert-warn" role="alert">
    {{ t("common.busy") }}
    <button v-if="onRetry" type="button" class="btn btn-sm" @click="onRetry()">{{ t("common.retry") }}</button>
  </div>
  <ErrorAlert v-else :error="error" :title="title" :on-retry="onRetry" />
</template>
