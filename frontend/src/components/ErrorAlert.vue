<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";
import { asApiError, canRetry, errorHeading, errorMessage } from "../lib/errors";

/** Human-readable explanation for any thrown error, with the API's details when present. */
const props = defineProps<{ error: unknown; title?: string; onRetry?: () => void }>();

const apiError = computed(() => asApiError(props.error));
const retry = computed(() => (canRetry(apiError.value) ? props.onRetry : undefined));
const heading = computed(() => props.title ?? errorHeading(apiError.value));
/** Details that only repeat the message (a guard naming one field) add nothing. */
const details = computed(() => (apiError.value?.details ?? []).filter((d) => d.message !== apiError.value?.message));
const message = computed(() => errorMessage(props.error));
</script>

<template>
  <div class="alert alert-error" role="alert">
    <strong>{{ heading }}</strong>
    <div>{{ message }}</div>
    <ul v-if="details.length > 0">
      <li v-for="(d, i) in details" :key="i">
        <code v-if="d.field && d.field !== '(root)'">{{ d.field }}</code> {{ d.message }}
      </li>
    </ul>
    <div v-if="apiError?.requestId || retry" class="meta">
      <template v-if="apiError?.requestId">{{ t("error.requestId") }} <code>{{ apiError.requestId }}</code>&#32;</template>
      <button v-if="retry" type="button" class="btn btn-sm" @click="retry()">{{ t("common.retry") }}</button>
    </div>
  </div>
</template>
