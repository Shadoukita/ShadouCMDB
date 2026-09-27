<script setup lang="ts">
import { computed } from "vue";
import { ApiError } from "../api/client";

/** Human-readable explanation for any thrown error, with the API's details when present. */
const props = defineProps<{ error: unknown; title?: string; onRetry?: () => void }>();

const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));
// Retrying a refused request cannot succeed until someone changes the user's profiles.
const retry = computed(() => (apiError.value?.code === "FORBIDDEN" ? undefined : props.onRetry));
const heading = computed(() => props.title ?? headingFor(apiError.value));
const message = computed(() => {
  const e = props.error;
  return e instanceof Error ? e.message : String(e);
});

function headingFor(e: ApiError | null): string {
  if (!e) return "Something went wrong";
  switch (e.code) {
    case "NETWORK_ERROR":
      return "API unreachable";
    case "DATABASE_UNAVAILABLE":
      return "The CMDB database is unavailable";
    case "SCHEMA_NOT_MIGRATED":
      return "The database is not migrated yet";
    case "NOT_FOUND":
      return "Not found";
    case "VALIDATION_ERROR":
      return "The API rejected the request";
    case "VERSION_CONFLICT":
      return "Someone else changed this record";
    case "CONFLICT":
    case "IN_USE":
      return "Conflict";
    case "FORBIDDEN":
    case "UNAUTHORIZED":
      return "Permission denied";
    default:
      return `Request failed (${e.status || e.code})`;
  }
}
</script>

<template>
  <div class="alert alert-error" role="alert">
    <strong>{{ heading }}</strong>
    <div>{{ message }}</div>
    <ul v-if="apiError && apiError.details.length > 0">
      <li v-for="(d, i) in apiError.details" :key="i">
        <code v-if="d.field && d.field !== '(root)'">{{ d.field }}</code> {{ d.message }}
      </li>
    </ul>
    <div v-if="apiError?.requestId || retry" class="meta">
      <template v-if="apiError?.requestId">Request id <code>{{ apiError.requestId }}</code>&#32;</template>
      <button v-if="retry" type="button" class="btn btn-sm" @click="retry()">Retry</button>
    </div>
  </div>
</template>
